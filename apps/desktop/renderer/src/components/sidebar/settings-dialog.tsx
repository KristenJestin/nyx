import { forwardRef, useCallback, useImperativeHandle, useState, type ComponentType } from "react";
import {
  errorText,
  nyxBridge,
  type AppSettings,
  type DetectedShell,
  type IntegrationStatus,
  type ShellChoice,
} from "@/bridge";
import {
  AlertTriangleIcon,
  CheckCircle2Icon,
  PlugIcon,
  SettingsIcon,
  TerminalIcon,
  XCircleIcon,
} from "lucide-react";

import { cn } from "@/lib/utils";
import { Button } from "@/components/ui/button";
import { Dialog, DialogBackdrop, DialogPopup } from "@/components/ui/dialog";
import { Spinner } from "@/components/ui/spinner";
import { toast } from "@/components/ui/toast";

// ---------------------------------------------------------------------------
// Provider card
// ---------------------------------------------------------------------------

interface ProviderCardProps {
  status: IntegrationStatus;
  /** Called after a successful install/remove so the list can be refreshed. */
  onRefresh: () => void;
}

/**
 * One integration row with a SINGLE Install/Uninstall button (findings #45/#49). The
 * nyx Claude integration is now ONE bundled plugin (MCP server + session-capture hooks),
 * so there is one control driving the whole integration — no separate MCP/plugin buttons.
 *
 * The install/remove side effect is owned HERE (per card), so the in-flight state stays
 * local: the button shows a loading spinner + disables while the op runs (it takes a few
 * seconds — copy + the `claude` CLI), and an error (claude absent / CLI failure) is
 * surfaced verbatim below the button. The card — and the whole modal — stays mounted, so
 * there is no full-pane flash (review finding 01KVAQAAW3D7TKPTVKMTBKDDPY).
 */
function ProviderCard({ status, onRefresh }: ProviderCardProps) {
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const run = useCallback(
    async (cmd: "integration_install" | "integration_remove") => {
      setBusy(true);
      setError(null);
      try {
        await nyxBridge.invoke(cmd, { provider: status.provider });
        onRefresh();
        toast.success(
          cmd === "integration_install"
            ? `${status.label} integration installed`
            : `${status.label} integration removed`,
        );
      } catch (e) {
        // The bridge serializes a backend `Err` into a BridgeError whose `message` is the original
        // backend string — read through the ONE path that knows that shape.
        const reason = errorText(e, "Operation failed");
        // Surface the real reason both inline (under the button) and as a toast.
        setError(reason);
        toast.error(reason);
      } finally {
        setBusy(false);
      }
    },
    [status.provider, onRefresh],
  );

  return (
    <div
      className={cn(
        "flex flex-col rounded-lg border px-4 py-3",
        status.available ? "border-border bg-card" : "border-border/50 bg-card/50 opacity-60",
      )}
    >
      {/* Header: label + (installed badge / coming soon badge) + the single action. */}
      <div className="flex items-center justify-between gap-2">
        <div className="flex items-center gap-2">
          <span className="text-sm font-medium text-card-foreground">{status.label}</span>
          {status.available && status.installed && (
            <span className="inline-flex items-center gap-1 rounded-full bg-success/12 px-2 py-0.5 text-2xs font-medium text-success">
              <CheckCircle2Icon className="size-3" />
              Installed
            </span>
          )}
        </div>
        {!status.available && (
          <span className="inline-flex items-center rounded-full bg-muted px-2 py-0.5 text-2xs font-medium text-muted-foreground">
            Coming soon
          </span>
        )}
        {status.available &&
          (status.installed ? (
            <Button
              variant="destructive-outline"
              size="xs"
              loading={busy}
              disabled={busy}
              onClick={() => void run("integration_remove")}
            >
              Uninstall
            </Button>
          ) : (
            <Button
              variant="default"
              size="xs"
              loading={busy}
              disabled={busy}
              onClick={() => void run("integration_install")}
            >
              Install
            </Button>
          ))}
      </div>

      {/* A measured problem with an INSTALLED integration — today, the MCP port divergence: the
          agent plugin is global to the machine, so it can be pointed at another nyx. Shown as a
          warning (not an error): the integration IS installed, it is just aimed elsewhere. This is
          the only place that mismatch is visible before it reaches an agent as ConnectionRefused. */}
      {status.diagnostic && (
        <p className="mt-2 flex items-start gap-1 text-xs text-warning">
          <AlertTriangleIcon className="mt-px size-3 shrink-0" />
          <span>{status.diagnostic}</span>
        </p>
      )}

      {error && (
        <p className="mt-2 flex items-center gap-1 text-xs text-destructive">
          <XCircleIcon className="size-3 shrink-0" />
          {error}
        </p>
      )}
    </div>
  );
}

// ---------------------------------------------------------------------------
// Integrations section
// ---------------------------------------------------------------------------

interface IntegrationsSectionProps {
  integrations: readonly IntegrationStatus[];
  /**
   * The failure of the section's READ (`integration_list`), already read through `errorText`, or
   * `null` when it answered.
   *
   * There is ONE read here, so this string doubles as the read's state: non-`null` means the list
   * on screen was never obtained, which is what keeps "nothing is installed" from being said about
   * a list nobody could read (see the rendering below and `TerminalSection`'s doc).
   *
   * That holds on EVERY read, first or not, because a read that fails DROPS the list it could not
   * refresh (`loadIntegrations`, same rule as `loadShells`' `setShells(null)`): a re-read is fired
   * on each modal open and after each install/remove, and the cards it would leave standing are
   * state the backend has just refused to confirm — with live Install/Uninstall buttons on it. So
   * `loadError` non-`null` always comes with an empty `integrations`; the two never contradict.
   */
  loadError: string | null;
  loading: boolean;
  onRefresh: () => void;
}

/**
 * The **Integrations** detail pane. Lists all supported MCP provider
 * integrations and threads each card the `onRefresh` callback so a successful
 * install/remove re-pulls the list.
 *
 * The list itself stays MOUNTED across an install/remove op — the per-card
 * spinner (see `ProviderCard`) covers the in-flight state. The list-level
 * spinner is reserved for the FIRST load (when there is nothing to show yet).
 *
 * A failed READ is a SENTENCE here too, on the same rule as `TerminalSection`: an unread list
 * rendered as "No integrations available." is the screen reporting a machine it never looked at,
 * and the two facts — "the list could not be read" and "this machine has no integration" — are
 * never allowed to pass for one another. Same rule on the same event, too: a read that fails leaves
 * NO list behind (see `loadIntegrations`), exactly as a failed `shell_list` leaves no menu — the
 * cards of a refused re-read are unconfirmed state carrying live Install/Uninstall buttons.
 */
function IntegrationsSection({
  integrations,
  loadError,
  loading,
  onRefresh,
}: IntegrationsSectionProps) {
  return (
    <section>
      <h3 className="text-base font-semibold text-foreground">Integrations</h3>
      <p className="mt-1 mb-4 text-sm text-muted-foreground">
        Connect nyx to AI coding tools. Installed integrations are kept up to date automatically on
        every launch.
      </p>

      {/* Only show the list-level spinner on the FIRST load (empty list). Once
          providers are on screen we keep them mounted and let the per-button
          loading state cover install/remove — no full-pane flash. */}
      {loading && integrations.length === 0 ? (
        <div className="flex items-center justify-center py-6">
          <Spinner className="size-5 text-muted-foreground" />
        </div>
      ) : (
        <ul className="flex flex-col gap-2">
          {integrations.map((s) => (
            <li key={s.provider}>
              <ProviderCard status={s} onRefresh={onRefresh} />
            </li>
          ))}
          {/* Said only about a list that ANSWERED: an empty list is a fact about the machine, and a
              read that failed established no such fact — it gets the line below instead. */}
          {!loadError && integrations.length === 0 && (
            <li className="py-4 text-center text-sm text-muted-foreground">
              No integrations available.
            </li>
          )}
          {loadError && (
            <li className="flex items-start gap-1 py-4 text-xs text-destructive">
              <XCircleIcon className="mt-0.5 size-3 shrink-0" />
              {loadError}
            </li>
          )}
        </ul>
      )}
    </section>
  );
}

// ---------------------------------------------------------------------------
// Terminal section — the default-shell preference
// ---------------------------------------------------------------------------

/** The `<option>` value standing for "Auto": no preference expressed at all. */
const AUTO_SHELL_VALUE = "";
/**
 * The `<option>` value standing for a preference the DETECTED LIST no longer holds.
 * It is not a choice the user can make (the option is disabled) — it exists so the select can
 * SHOW the orphaned preference instead of silently snapping back to "Auto", which would read as
 * "nyx forgot what I picked" rather than "the shell you picked is not on offer any more".
 *
 * What it may say is bounded by what `shell_list` establishes, which is membership of that list and
 * NOTHING ELSE — not that the binary was uninstalled, and not what the next spawn will do. Saying
 * "no longer installed / new terminals fall back to the system default" would be two further
 * claims, neither of them this read's to make.
 *
 * The WSL case used to be a genuine CONTRADICTION rather than merely an over-claim, and this
 * comment used to record it as such: a preference whose distribution had been unregistered left
 * `wsl.exe` on disk, so `resolve` (`shells.rs`) preferred it and spawned a terminal that died on
 * "There is no distribution with the supplied name". That hole is closed (review
 * `01M0BT2SST6ZMG84SW1CW9BSWH`): `resolve` now validates the PAIR — the program and, for a WSL
 * choice, the distribution — against the same probe this list is built from, so a preference the
 * machine has denied loses to the historical chain instead of failing the spawn.
 *
 * It is about the list, so it may only be used when that list was really read — see
 * {@link UNVERIFIED_SHELL_VALUE} for the case where it was not.
 */
const MISSING_SHELL_VALUE = "__missing__";
/**
 * The `<option>` value standing for a preference that could not be CHECKED against the machine:
 * `app_settings_get` answered (so nyx knows what was picked) while `shell_list` did not (so nyx
 * knows nothing about what the machine still has).
 *
 * It exists because the two other outcomes would both be a claim nobody verified: falling back to
 * "Auto" says "nyx forgot what I picked", and {@link MISSING_SHELL_VALUE} says "the machine's list
 * no longer holds it" — which no probe established here, and which the sidecar would contradict at
 * the next spawn anyway, since it resolves the stored preference against the machine itself (the
 * program on disk, plus the WSL distribution where the choice names one), not against this
 * screen's `shell_list` response. So the preference is shown as itself, and the unread list is
 * stated as a failure line instead.
 */
const UNVERIFIED_SHELL_VALUE = "__unverified__";
/**
 * The `<option>` value standing for a preference nyx could not even READ: `app_settings_get` did
 * not answer, so NOTHING is known about what the user picked.
 *
 * It is the other half of the same rule as {@link UNVERIFIED_SHELL_VALUE}, applied to the OTHER
 * read. "Auto" is not a neutral placeholder — it is the statement "nothing was ever picked", a fact
 * about the preference — so rendering it here would have the screen answer, on nyx's behalf, the
 * one question the failed read left open. The failure line says what happened; this option says
 * that the answer is unknown, and nothing more.
 */
const UNREAD_PREFERENCE_VALUE = "__unread__";

/**
 * The `<option>` values that are DISPLAY ONLY — each names a state of the reads, never a shell the
 * user could pick. They are the values `choose` must refuse (see there); "Auto" is NOT one of them,
 * since clearing the preference is a real gesture.
 */
const SENTINEL_VALUES: readonly string[] = [
  MISSING_SHELL_VALUE,
  UNVERIFIED_SHELL_VALUE,
  UNREAD_PREFERENCE_VALUE,
];

/** Do two shell choices name the same thing? Paths are compared case-insensitively because
 *  Windows paths are (`C:\Windows` and `c:\windows` are one file), and the fixed args must match
 *  too — two WSL entries differ only by their `-d <distro>`. */
function sameShell(a: ShellChoice, b: ShellChoice): boolean {
  return (
    a.path.toLowerCase() === b.path.toLowerCase() &&
    a.args.length === b.args.length &&
    a.args.every((arg, i) => arg === b.args[i])
  );
}

/**
 * How a stored choice READS on screen, when there is no detected entry to borrow a label from.
 *
 * A shell IS its program AND the fixed args that make it that shell — `sameShell` compares the
 * pair, the backend stores the pair, {@link ShellChoice} carries the pair — so both halves are
 * named. `C:\Windows\System32\wsl.exe` alone names no WSL distribution: it is the path of EVERY
 * one of them, including entries still live in the very same menu.
 */
function describeChoice(choice: ShellChoice): string {
  return [choice.path, ...choice.args].join(" ");
}

/**
 * The OUTCOME of the preference read (`app_settings_get`) — which is not the same thing as the
 * preference itself, and that difference is the whole point of the wrapper.
 *
 * The state is carried as `ShellPreference | null`: `null` is "the read did NOT answer" (nyx knows
 * nothing), `{ choice: null }` is "it answered: nothing was ever picked" (the real "Auto"), and
 * `{ choice }` is "it answered: this is the pick". Exactly the tri-state `shells` already carries
 * for the other read — an unread value is not a value, and the section must never let one pass for
 * the other.
 */
interface ShellPreference {
  readonly choice: ShellChoice | null;
}

/**
 * WHICH `<option>` the select sits on — the section's whole "say only what was read" rule, in one
 * place. Five outcomes, deliberately kept apart:
 *
 * - the preference read did not answer → {@link UNREAD_PREFERENCE_VALUE} (nothing is known);
 * - it answered "nothing picked" → "Auto", the only case where that word is a fact;
 * - the pick is in the detected list → that entry;
 * - the pick is in NO list because the list did not answer → {@link UNVERIFIED_SHELL_VALUE};
 * - the pick is in a list that ANSWERED and does not hold it → {@link MISSING_SHELL_VALUE}.
 */
function selectedShellOption(
  preference: ShellPreference | null,
  shells: readonly DetectedShell[] | null,
): string {
  if (preference === null) return UNREAD_PREFERENCE_VALUE;
  const chosen = preference.choice;
  if (chosen === null) return AUTO_SHELL_VALUE;
  const matched = shells?.find((shell) => sameShell(shell, chosen));
  if (matched) return matched.id;
  return shells === null ? UNVERIFIED_SHELL_VALUE : MISSING_SHELL_VALUE;
}

interface TerminalSectionProps {
  /**
   * The shells the host really has (`shell_list`) — every one of them spawnable — or `null` when
   * that probe did NOT answer.
   *
   * The `null` is the whole point of the type: an unread list is not an empty one, and the section
   * decides two things on the difference (whether a stored preference can be called missing, and
   * whether the machine can be called shell-less). An empty array is a MEASURED emptiness.
   */
  shells: readonly DetectedShell[] | null;
  /**
   * The persisted preference AS ITS READ LEFT IT (`app_settings_get`) — see {@link ShellPreference}:
   * `null` when that read did NOT answer, `{ choice: null }` when it answered "nothing expressed"
   * (the real "Auto"), `{ choice }` when it answered with the stored pick.
   *
   * The SAME tri-state as `shells` above, on the section's other read, for the same reason: an
   * unread preference rendered as "Auto" is the screen stating that the user picked nothing —
   * a fact about the preference, established by nobody, on the half nobody could read.
   */
  preference: ShellPreference | null;
  /**
   * The failures of the section's own READS (`shell_list` / `app_settings_get`), already read
   * through `errorText` — empty when both answered.
   *
   * ONE ENTRY PER READ, and that is why it is a list rather than a string: the two reads fail for
   * unrelated reasons, so their messages are two independent sentences. Joined by a space they made
   * one unreadable run-on — "Could not read the shells installed on this machine Could not read the
   * saved default shell", or the sidecar's own sentence printed twice back to back — which is not
   * the readable message a failed read owes the user. The section renders one line each.
   *
   * They are PROPS rather than local state because the reads are the dialog's (one open-event
   * reload pulls the whole modal), while the write below is this section's — and both must land
   * in the same inline place, because on screen there is one Terminal section.
   */
  loadErrors: readonly string[];
  loading: boolean;
  /** Called after a successful write so the modal re-pulls the persisted truth. */
  onRefresh: () => void;
}

/**
 * The **Terminal** detail pane: WHICH shell nyx opens its terminals on.
 *
 * The model is Windows Terminal's profile menu — a list DISCOVERED from the machine rather than a
 * vocabulary the user has to match — so the options come from the backend's `shell_list` probe and
 * every one of them is a shell that really exists (the probe drops any candidate whose binary is
 * absent). "Auto" leads the list and is not a shell: it is the ABSENCE of a preference, which is
 * what keeps nyx on its historical resolution (`$SHELL`, then the per-OS default).
 *
 * The write is owned HERE, per the `ProviderCard` pattern: the select disables while the mutation
 * is in flight and an error is surfaced inline rather than swallowed. The pane stays mounted, so
 * there is no full-pane flash — the list-level spinner is reserved for the FIRST load.
 *
 * A failure of the READS reaches the same inline place, through `loadErrors`. That is not symmetry
 * for its own sake: this section's copy CLAIMS to list the shells installed on the machine, so an
 * unread list rendered as an empty menu is the screen asserting something it never read — the
 * defect that made the previous one undebuggable from the outside (no message, no clue). "The read
 * failed" and "this machine has no other shell" are therefore rendered as two distinct sentences.
 *
 * The two reads are also KEPT APART here, not merged into one "something failed" flag: each of this
 * section's claims is gated on the read that backs it, and BOTH carry the same tri-state (`shells`
 * is `null` when nobody looked, `preference` is `null` when nobody read it). Only a list that
 * answered can say a stored preference is no longer detected, and only a list that answered can
 * call the machine shell-less — so a `shell_list` that failed while `app_settings_get` succeeded no
 * longer produces "no longer detected" out of thin air, and an empty list stays named as empty even
 * when the preference read is the half that failed. Symmetrically, only a preference read that
 * answered can put the select on "Auto", which is the statement "nothing was ever picked" and not a
 * neutral resting place: with both reads down — a missing sidecar, the most ordinary failure of the
 * two — the select names the preference as UNKNOWN rather than reporting a choice the user never
 * made.
 *
 * Scope, stated on screen because it is the one thing a user would otherwise test by hand: the
 * choice applies to terminals opened FROM NOW ON. The sidecar resolves the preference at each
 * spawn, so the terminals already running are deliberately left alone (nyx never yanks a live
 * shell out from under a working session).
 */
function TerminalSection({
  shells,
  preference,
  loadErrors,
  loading,
  onRefresh,
}: TerminalSectionProps) {
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  /** The options the menu can offer: an unread list offers none — it does not offer "nothing". */
  const detected = shells ?? [];

  // WHICH option is selected — the five outcomes of the two reads, kept apart (see
  // `selectedShellOption`). This is not a plain `preference?.choice.id` because three of those
  // outcomes have no detected entry to point at, and each says a DIFFERENT thing about what is
  // known: the pick is gone from a list that answered, the list never answered, or the pick itself
  // was never read.
  const stored = preference?.choice ?? null;
  const selected = selectedShellOption(preference, shells);

  const choose = useCallback(
    async (value: string) => {
      // A SENTINEL IS NOT A CHOICE — the rule stated where it applies, rather than left to the
      // `disabled` attribute of the options below. Those values name a state of the reads (gone
      // from the list / list unread / preference unread), and `shells.find` returns `undefined` for
      // every one of them — which the write below reads as "Auto" and persists as `path: null`.
      // Submitting one would therefore ERASE the very preference the option exists to display, and
      // report it as a success. The `disabled` attribute keeps the browser from sending them; this
      // keeps the erasure impossible even if it ever went missing.
      if (SENTINEL_VALUES.includes(value)) return;
      // The renderer never composes a command line: it hands back the entry `shell_list` produced
      // (path AND args — a WSL distribution is `wsl.exe -d <distro>`, unnameable by path alone).
      const picked = shells?.find((s) => s.id === value);
      setBusy(true);
      setError(null);
      try {
        await nyxBridge.invoke("set_default_shell", {
          path: picked ? picked.path : null,
          args: picked ? [...picked.args] : [],
        });
        onRefresh();
        toast.success(
          picked
            ? `New terminals will open ${picked.label}`
            : "New terminals will use the system default",
        );
      } catch (e) {
        // The bridge rejects with a BridgeError OBJECT — read it through the one path that knows
        // that shape, or the user gets `[object Object]`.
        const reason = errorText(e, "Could not save the default shell");
        setError(reason);
        toast.error(reason);
      } finally {
        setBusy(false);
      }
    },
    [shells, onRefresh],
  );

  // The section's failures, in ONE inline place but as SEPARATE statements — one line each,
  // whichever half raised them: the WRITE owned here, then the READS owned by the dialog's loader.
  // All arrive already read through `errorText`, so none can reach the screen as `[object Object]`.
  // The write leads because it answers a gesture the user just made, while a read failure is the
  // state they walked into. Concatenating them was the defect: two unrelated sentences glued by a
  // space read as one broken sentence, exactly when the user has the least to go on.
  const failures = error ? [error, ...loadErrors] : loadErrors;

  return (
    <section>
      <h3 className="text-base font-semibold text-foreground">Terminal</h3>
      <p className="mt-1 mb-4 text-sm text-muted-foreground">
        Choose the shell nyx opens its terminals on. Only shells installed on this machine are
        listed. The choice applies to terminals opened from now on — the ones already running keep
        their shell.
      </p>

      {/* The FIRST load is the only one that hides the control: once the options are on screen we
          keep them mounted and let the select's own disabled state cover the in-flight write. */}
      {loading && detected.length === 0 ? (
        <div className="flex items-center justify-center py-6">
          <Spinner className="size-5 text-muted-foreground" />
        </div>
      ) : (
        <div className="flex flex-col gap-2">
          <label className="text-xs font-medium text-muted-foreground" htmlFor="default-shell">
            Default shell
          </label>
          <select
            id="default-shell"
            aria-label="Default shell"
            disabled={busy}
            value={selected}
            onChange={(e) => void choose(e.target.value)}
            className="w-full rounded-md border border-input bg-popover px-2 py-1.5 text-sm text-foreground outline-none focus-visible:ring-2 focus-visible:ring-ring disabled:opacity-60"
          >
            <option value={AUTO_SHELL_VALUE}>Auto (system default)</option>
            {detected.map((shell) => (
              <option key={shell.id} value={shell.id}>
                {shell.label}
              </option>
            ))}
            {/* The orphaned preference: shown so the user can SEE what they had picked, disabled
                so it cannot be re-picked (the detected list no longer holds it). Named in FULL —
                program AND args — because `wsl.exe` alone is the path of every WSL distribution,
                including ones still listed live above. */}
            {selected === MISSING_SHELL_VALUE && stored && (
              <option value={MISSING_SHELL_VALUE} disabled>
                {describeChoice(stored)} (no longer detected)
              </option>
            )}
            {/* The UNCHECKED preference: the same "show what was picked" gesture, minus the verdict
                the failed probe cannot support — nothing found it missing, so nothing is said. */}
            {selected === UNVERIFIED_SHELL_VALUE && stored && (
              <option value={UNVERIFIED_SHELL_VALUE} disabled>
                {describeChoice(stored)}
              </option>
            )}
            {/* The UNREAD preference: there is nothing to show, so the option says exactly that.
                Falling back to "Auto" here would answer for the read that failed.

                It is named in the section's OWN words — "the saved default shell", the same noun the
                failure line below uses for the same read ("Could not read the saved default shell").
                "Preference" is vocabulary from the props and the constants, and no sentence on this
                screen uses it. The shape stays the one the menu already speaks — `<name> (<plain
                English gloss>)`, as in "Auto (system default)". */}
            {selected === UNREAD_PREFERENCE_VALUE && (
              <option value={UNREAD_PREFERENCE_VALUE} disabled>
                Unknown (the saved default shell could not be read)
              </option>
            )}
          </select>

          {/* Only ever said when the LIST answered: it is a statement about the machine, and a probe
              that failed made no statement about anything. And it says only what that list
              establishes — the entry is not in it. It does NOT say the binary was uninstalled (a
              WSL preference whose distribution was removed leaves wsl.exe right where it was), nor
              what the next spawn will run: that verdict is the sidecar's, re-taken against the
              machine at spawn time (`resolve`, `shells.rs`), not a conclusion this snapshot of the
              list is entitled to draw. */}
          {selected === MISSING_SHELL_VALUE && (
            <p className="flex items-start gap-1 text-xs text-warning">
              <AlertTriangleIcon className="mt-0.5 size-3 shrink-0" />
              {/* The ADVICE is gated on there being something to take it: the menu offers "Auto" and
                  the disabled orphan alone when the list answered EMPTY, so "pick another one" would
                  point at an option that is not there. The DIAGNOSIS — the half backed by the list —
                  is said either way. */}
              {detected.length > 0
                ? "This shell is no longer in the list detected on this machine — pick another one to be sure new terminals open."
                : "This shell is no longer in the list detected on this machine."}
            </p>
          )}

          {failures.map((message, index) => (
            <p
              // Two reads can fail with the SAME sentence (one bridge down answers both), so the
              // message alone is not an identity — its rank in the list is.
              key={`${index}-${message}`}
              className="flex items-start gap-1 text-xs text-destructive"
            >
              <XCircleIcon className="mt-0.5 size-3 shrink-0" />
              {message}
            </p>
          ))}

          {/* "The read failed" and "this machine offers nothing else" are two DIFFERENT facts and
              the section must never let one pass for the other: a failed read is the line above,
              a genuinely empty probe is this one. Without it, an empty menu under a paragraph
              claiming to list the installed shells is a screen asserting something it never read.
              It is gated on THIS read — `shells !== null`, the probe answered — and not on "nothing
              failed": when the OTHER half is the one that failed, the machine is still legitimately
              shell-less and saying so is still the truth.

              TWO claims, and only the FIRST is the list's to make. An empty `shell_list` is a fact
              about the MENU; what the next terminal opens on is decided by `resolve` (`shells.rs`),
              which prefers the STORED choice whenever that choice still names an openable shell —
              a test this probe never ran. The two co-occur for real: on Unix an unreadable
              `/etc/shells` makes
              `detect_candidates` answer `Vec::new()` (a measured emptiness), so a machine with a
              stored `/usr/bin/fish` lands here while the next spawn is still fish. The fallback half
              is therefore said only where it IS established — `AUTO_SHELL_VALUE`, i.e. the
              preference read ANSWERED and answered "nothing picked", the one state in which
              `resolve` has no preference to prefer and falls through to `$SHELL` / the per-OS
              default. Anything else (an orphaned pick, an unread one) leaves the sentence at what
              the list alone supports. */}
          {shells !== null && detected.length === 0 && (
            <p className="text-xs text-muted-foreground">
              {selected === AUTO_SHELL_VALUE
                ? "No shell was detected on this machine — new terminals open on the system default."
                : "No shell was detected on this machine."}
            </p>
          )}
        </div>
      )}
    </section>
  );
}

// ---------------------------------------------------------------------------
// Section registry (the left rail)
// ---------------------------------------------------------------------------

type SectionId = "integrations" | "terminal";

interface SectionDef {
  id: SectionId;
  label: string;
  icon: ComponentType<{ className?: string }>;
}

/**
 * The Settings sections, in rail order. Adding a future section (e.g. Appearance,
 * General) is a matter of appending a `SectionDef` here and rendering its pane in
 * the detail switch below — the rail, selection state and navigation are generic.
 */
const SECTIONS: readonly SectionDef[] = [
  { id: "integrations", label: "Integrations", icon: PlugIcon },
  { id: "terminal", label: "Terminal", icon: TerminalIcon },
];

// ---------------------------------------------------------------------------
// Settings dialog
// ---------------------------------------------------------------------------

export interface SettingsDialogProps {
  open: boolean;
  onClose: () => void;
}

/**
 * Imperative handle exposed by `<SettingsDialog>` so the *event* that opens the
 * modal (the gear-button handler) can pull a fresh provider list. We deliberately
 * do NOT reload from a `useEffect` watching `open`: that fakes an event handler
 * with an effect (an extra render that runs late, flagged by react-doctor's
 * `no-event-handler`). The open path owns the side effect instead — see
 * `terminal-manager`'s `onOpenSettings`.
 */
export interface SettingsDialogHandle {
  /** Re-pull everything the modal displays (the integration list AND the Terminal section's
   *  detected shells + persisted preference) from the backend. */
  reload: () => void;
}

/**
 * `<SettingsDialog>` — the global Settings modal (triggered from the gear icon
 * in the sidebar head).
 *
 * LAYOUT: a **left section rail** (the `SECTIONS` registry — Integrations and
 * Terminal, extensible) + a **right detail pane** that renders the selected
 * section. This mirrors the Commands-modal navigation pattern (a rail of
 * selectable entries + a detail surface), rendered with nyx's design tokens.
 *
 * The **Terminal** section owns the app-wide default-shell preference: a picker over the shells
 * `shell_list` discovered on the host, persisted through `set_default_shell` and read back through
 * `app_settings_get`. It applies to terminals opened afterwards — see `TerminalSection`.
 *
 * The **Integrations** section lists all supported provider integrations. Each
 * available provider has a SINGLE Install/Uninstall control (findings #44/#45/#46): the
 * nyx Claude integration is now ONE bundled plugin that registers a local marketplace +
 * an `enabledPlugins` reference in `~/.claude/settings.json` and bundles BOTH the nyx MCP
 * server (its `.mcp.json`) AND the SessionStart/SessionEnd session-capture hooks. Install
 * copies + registers the plugin via the `claude` CLI (and strips any legacy standalone
 * MCP); uninstall removes the plugin + all nyx residue. Codex / OpenCode / Custom are
 * coming soon (UI disabled, no actions).
 *
 * Install state is derived from Claude Code's REAL config by the Rust backend; this
 * component only reads and triggers mutations via the sidecar's `integration_list` /
 * `integration_install` / `integration_remove` commands.
 *
 * The dialog stays mounted across open/close (its exit is Motion-animated, so an
 * immediate unmount would cut the animation short). The provider list is loaded
 * on the OPEN EVENT via the imperative `reload()` handle, not via an effect that
 * watches the `open` prop.
 */
export const SettingsDialog = forwardRef<SettingsDialogHandle, SettingsDialogProps>(
  function SettingsDialog({ open, onClose }, ref) {
    const [integrations, setIntegrations] = useState<readonly IntegrationStatus[]>([]);
    /** The READ failure of the Integrations section, already read through `errorText` — `null` when
     *  the list answered. Same rule as `shellsErrors`: a refusal is said, never swallowed. */
    const [integrationsError, setIntegrationsError] = useState<string | null>(null);
    const [loading, setLoading] = useState(false);
    const [activeSection, setActiveSection] = useState<SectionId>("integrations");
    // The Terminal section's two reads: WHAT the machine offers (`shell_list`) and WHAT the user
    // chose (`app_settings_get`). Held here, next to the integration list, so the ONE open-event
    // reload refreshes the whole modal — a section that pulled on its own mount would be an effect
    // watching `open` by another name.
    // `null` is "not read" — the state `shell_list` leaves behind when it rejects, and the one an
    // empty array must never stand in for (see `TerminalSection`'s `shells` prop).
    const [shells, setShells] = useState<readonly DetectedShell[] | null>(null);
    /** The preference read's OUTCOME, on the same `null` = "not read" convention as `shells` above:
     *  `{ choice: null }` is the answer "nothing was ever picked", which only a read that ANSWERED
     *  can produce (see `ShellPreference`). */
    const [preference, setPreference] = useState<ShellPreference | null>(null);
    const [shellsLoading, setShellsLoading] = useState(false);
    /** The READ failures of the Terminal section, already read through `errorText` — ONE PER READ,
     *  empty when both answered. Held next to the values they invalidate so the section can tell
     *  "the read failed" from "this machine has no shell", and kept apart so two failures stay two
     *  sentences on screen instead of one run-on. */
    const [shellsErrors, setShellsErrors] = useState<readonly string[]>([]);

    const loadIntegrations = useCallback(async () => {
      setLoading(true);
      try {
        const list = await nyxBridge.invoke<IntegrationStatus[]>("integration_list");
        setIntegrations(list);
        setIntegrationsError(null);
      } catch (e) {
        // The list is DROPPED, not kept — the same answer `loadShells` gives below (`setShells`
        // `(null)`), on the same event. This is a RE-read as often as a first one (every modal open,
        // and every install/remove through `onRefresh`), and the cards of a re-read that failed are
        // a machine state the backend just refused to confirm, left on screen with live
        // Install/Uninstall buttons under a red line. Two sections cannot answer one event two
        // opposite ways.
        setIntegrations([]);
        // The refusal becomes a SENTENCE, on the same rule the Terminal section applies below: this
        // catch used to be empty, and a swallowed `integration_list` was indistinguishable on screen
        // from a machine with no integration at all. `errorText` is the one path that reads a
        // BridgeError (an OBJECT) without turning it into `[object Object]`.
        setIntegrationsError(errorText(e, "Could not read the integrations on this machine"));
      } finally {
        setLoading(false);
      }
    }, []);

    const loadShells = useCallback(async () => {
      setShellsLoading(true);
      try {
        // The two reads are fired together but SETTLED INDEPENDENTLY, and that is the whole point
        // (review finding `01M083NBZW4ZXKNSGDTS7F14Y8`). They answer two DIFFERENT questions —
        // WHAT the machine offers (a filesystem/`PATH`/WSL probe, no database) and WHAT the user
        // chose (a row in `app_settings`) — and they fail for unrelated reasons. Under
        // `Promise.all` the first rejection discarded BOTH: measured on this machine, a database
        // whose `app_settings` table was missing made `app_settings_get` reject, which threw away
        // a `shell_list` that had already answered with five real shells, and the picker showed
        // "Auto" alone. `allSettled` keeps the half that answered.
        const [list, settings] = await Promise.allSettled([
          nyxBridge.invoke<DetectedShell[]>("shell_list"),
          nyxBridge.invoke<AppSettings>("app_settings_get"),
        ]);

        // Each half is read on its own, and its failure becomes a SENTENCE rather than a silence
        // (review finding `01M083P95W52AC7RWMNFV5GTG9`) — through `errorText`, the one path that
        // knows a BridgeError is an object and not an `Error`.
        const failures: string[] = [];
        if (list.status === "fulfilled") {
          // Defensive: a backend that answers nothing (an old sidecar, a doubled bridge in a test)
          // degrades to "no shell detected", never to a crash inside a modal.
          setShells(Array.isArray(list.value) ? list.value : []);
        } else {
          // `null`, never `[]`: an unread list must not reach the section as a machine that
          // offers nothing — the section would then call a stored preference uninstalled, a fact
          // this failed probe established in no way at all.
          setShells(null);
          failures.push(
            errorText(list.reason, "Could not read the shells installed on this machine"),
          );
        }
        if (settings.status === "fulfilled") {
          setPreference({ choice: settings.value?.defaultShell ?? null });
        } else {
          // `null`, never `{ choice: null }` — the same rule as `setShells(null)` just above, on
          // this section's other read. `{ choice: null }` is the ANSWER "nothing was ever picked",
          // which the select renders as "Auto"; a read that rejected answered nothing at all, and
          // must not have the screen say the user chose the system default on its behalf.
          setPreference(null);
          failures.push(errorText(settings.reason, "Could not read the saved default shell"));
        }
        // Kept as a LIST — one entry per read that did not answer. Joining them into a single
        // string is what produced the run-on the section now cannot render.
        setShellsErrors(failures);
      } catch (e) {
        // Nothing above rejects — `allSettled` absorbs both reads — so reaching here means the
        // bridge itself could not even start a call. Say so; never leave the section asserting a
        // list it never read.
        setShells(null);
        setPreference(null);
        setShellsErrors([errorText(e, "Could not read the terminal settings")]);
      } finally {
        setShellsLoading(false);
      }
    }, []);

    // Expose the reload to the open-event handler so opening the modal pulls fresh state (every
    // open), driven by the event rather than an effect.
    const reload = useCallback(() => {
      void loadIntegrations();
      void loadShells();
    }, [loadIntegrations, loadShells]);
    useImperativeHandle(ref, () => ({ reload }), [reload]);

    return (
      <Dialog.Root
        open={open}
        onOpenChange={(next) => {
          if (!next) onClose();
        }}
      >
        <Dialog.Portal>
          <DialogBackdrop />
          <DialogPopup className="flex max-h-[calc(100vh-4rem)] w-[min(44rem,calc(100vw-2rem))] flex-col overflow-hidden p-0">
            {/* Header */}
            <div className="flex items-center gap-2.5 border-b border-border px-5 py-4">
              <SettingsIcon className="size-4 shrink-0 text-muted-foreground" />
              <Dialog.Title className="text-base font-semibold">Settings</Dialog.Title>
            </div>

            {/* Body: left section rail + right detail pane. */}
            <div className="flex min-h-0 flex-1">
              {/* Left rail of sections (Integrations now, extensible). */}
              <nav
                aria-label="Settings sections"
                className="w-44 shrink-0 border-r border-border bg-muted/30 p-2"
              >
                <ul className="flex flex-col gap-0.5">
                  {SECTIONS.map((section) => {
                    const Icon = section.icon;
                    const selected = activeSection === section.id;
                    return (
                      <li key={section.id}>
                        <button
                          type="button"
                          aria-current={selected ? "page" : undefined}
                          onClick={() => setActiveSection(section.id)}
                          className={cn(
                            "flex w-full cursor-pointer items-center gap-2 rounded-md px-2.5 py-1.5 text-left text-sm font-medium outline-none transition-colors",
                            "focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-1 focus-visible:ring-offset-background",
                            selected
                              ? "bg-secondary text-secondary-foreground"
                              : "text-muted-foreground hover:bg-muted hover:text-foreground",
                          )}
                        >
                          <Icon className="size-4 shrink-0 opacity-80" />
                          {section.label}
                        </button>
                      </li>
                    );
                  })}
                </ul>
              </nav>

              {/* Right detail pane: the selected section. */}
              <div className="min-h-0 flex-1 overflow-y-auto px-5 py-5">
                {activeSection === "integrations" && (
                  <IntegrationsSection
                    integrations={integrations}
                    loadError={integrationsError}
                    loading={loading}
                    onRefresh={() => void loadIntegrations()}
                  />
                )}
                {activeSection === "terminal" && (
                  <TerminalSection
                    shells={shells}
                    preference={preference}
                    loadErrors={shellsErrors}
                    loading={shellsLoading}
                    onRefresh={() => void loadShells()}
                  />
                )}
              </div>
            </div>

            {/* Footer */}
            <div className="flex justify-end border-t border-border px-5 py-3.5">
              <Dialog.Close
                render={
                  <Button variant="outline" size="sm">
                    Close
                  </Button>
                }
                onClick={onClose}
              />
            </div>
          </DialogPopup>
        </Dialog.Portal>
      </Dialog.Root>
    );
  },
);
