//! OSC 133 (shell integration / command lifecycle) — the PORTABLE exec-state
//! source (ADR-0002).
//!
//! # Gate decision (PRD 2.1, task #1 — "Gate OSC 133 shell integration")
//!
//! This module is the artifact of the OSC 133 spike. It records the validated
//! production strategy AND provides the pure parser the bridge pump will use in
//! phase 2 to turn raw PTY bytes into terminal exec-state transitions
//! (`idle` → `running` → `success`/`error`). It is wired into the output pump in
//! a later phase; for now it is a documented, unit-tested, compilable decision —
//! the same "inline ADR in the owning module" convention `db.rs` uses for
//! ADR-0001.
//!
//! ## What OSC 133 is
//!
//! OSC 133 is the de-facto "shell integration" / "FinalTerm" command-lifecycle
//! protocol consumed by VS Code's terminal, Windows Terminal, iTerm2, WezTerm and
//! Kitty. A shell, instrumented with a few prompt hooks, emits these OSC
//! sequences around every command (terminator is `BEL` = `0x07` or the 2-byte
//! `ST` = `ESC \`, exactly like OSC 7):
//!
//! ```text
//! ESC ] 133 ; A ST          prompt start
//! ESC ] 133 ; B ST          command start (end of prompt / start of typed input)
//! ESC ] 133 ; C ST          pre-exec — the command is now running (output begins)
//! ESC ] 133 ; D ; <exit> ST command end, carrying the exit status
//! ```
//!
//! The terminal does not render these bytes — they are control sequences, so a
//! compliant emulator (xterm.js is one) consumes them invisibly. That is the
//! whole reason this is the production path and not a `/proc` heuristic: it is
//! **portable** (Windows/macOS/Linux), and it is the only mechanism that can
//! report a real **exit code** (success vs error), which `/proc` foreground-PID
//! sniffing fundamentally cannot.
//!
//! ## nyx mapping (what the bridge state machine consumes — phase 3)
//!
//! - `C` (pre-exec) → `running`. (We deliberately key "running" off `C`, not
//!   `B`: `B` only means the prompt finished and input may begin; a bare Enter at
//!   an empty prompt emits `B` then `D` with no `C`, and must NOT flash running.)
//! - `D;0` → `success`; `D;<non-zero>` → `error`; `D` with a missing/garbage code
//!   → settle to a result with `exit_code = None` (treated as error-ish by the
//!   state machine, never left as a stale `running`).
//! - `A`/`B` carry no exec-state meaning for nyx and are parsed-then-ignored
//!   (kept in the vocabulary so the parser stays robust to a full prompt stream).
//!
//! ## Injection strategy (validated; implemented in phase 2)
//!
//! nyx OWNS the shell it spawns (`pty.rs::resolve_shell` + `CommandBuilder`), so
//! it injects the integration itself rather than depending on the user having
//! configured shell integration. The injection must be **non-destructive**: it
//! must not clobber the user's prompt or startup files. The validated per-shell
//! strategy:
//!
//! - **bash** — spawn with `--rcfile <nyx-snippet>`; the snippet `source`s the
//!   user's `~/.bashrc` first, THEN appends `PROMPT_COMMAND` (precmd → emits
//!   `133;D;$?` then `133;A`) and a `DEBUG` trap (preexec → emits `133;C`). This
//!   is exactly VS Code's bash integration shape. Append, never replace, so the
//!   user prompt survives.
//! - **zsh** — point `ZDOTDIR` at a temp dir whose `.zshrc` sources the real
//!   user `.zshrc` then registers `precmd`/`preexec` hook functions (zsh has
//!   first-class hook arrays — cleaner than bash, no DEBUG trap). The native
//!   `precmd` reads `$?` for the exit code.
//! - **PowerShell** (pwsh 7 AND Windows PowerShell 5.1) — spawn with
//!   `-NoExit -Command "<dot-source nyx snippet>"`, or inject via the profile,
//!   wrapping the existing `prompt` function. The wrapper emits `133;D;<code>`
//!   (code from `$?` → `0`, else `$LASTEXITCODE` or `1`) + `133;A` at the top of
//!   `prompt`, and `133;B` at its end. `PSReadLine`'s `OnViMode`/command-accepted
//!   path can additionally emit `133;C`. See the PowerShell verdict below.
//!
//! ## PowerShell verdict (the make-or-break case — nyx's primary dev platform)
//!
//! **GO.** Empirically validated on this machine (Windows PowerShell 5.1,
//! `powershell.exe`; `pwsh.exe` not installed here but the `prompt`-function
//! mechanism is edition-identical). A `prompt`-function wrapper emits the OSC 133
//! command-end + prompt-start sequences **with no visible pollution**, and the
//! **exit code is recovered correctly**: a `prompt` after `exit 0` produced
//! `ESC]133;D;0 BEL`, and after `exit 3` produced `ESC]133;D;3 BEL`
//! (`$? → 0 else $LASTEXITCODE`). This is the same mechanism Windows Terminal /
//! VS Code use for PowerShell. No fallback is required for PowerShell.
//!
//! Raw probe capture (escapes shown):
//! ```text
//! ...<ESC>]133;D;0<BEL><ESC>]133;A<BEL>PS D:\Projects\nyx> <ESC>]133;B<BEL>   # after exit 0
//! ...<ESC>]133;D;3<BEL><ESC>]133;A<BEL>PS D:\Projects\nyx> <ESC>]133;B<BEL>   # after exit 3
//! ```
//!
//! ## Fallback / explicit degradation (unsupported shells)
//!
//! If the resolved shell is not bash/zsh/PowerShell (e.g. `cmd.exe`, `sh`, fish,
//! nushell, or a shell whose integration injection failed), nyx injects NOTHING
//! and the terminal stays **`idle`-only**: with no `133;C`/`133;D` in the stream
//! the state machine never transitions, so there is **no false `running`** and no
//! fake success/error. Honest degradation, per the PRD. `cmd.exe` has no prompt
//! hook to carry OSC reliably and is therefore an idle-only shell by design.
//!
//! ## Why not `/proc` (rejected, per PRD)
//!
//! Linux `/proc/<pgid>/comm` foreground-process sniffing is non-portable (no
//! `/proc` on Windows/macOS) and CANNOT produce an exit code, so it can never
//! tell success from error. It is explicitly NOT the production path. (`/proc`
//! stays only as the live-cwd source in `crate::proc`, unrelated to exec-state.)
//!
//! ## Purity
//!
//! Like `crate::osc7`, this module is pure (no IO): it takes a byte slice and
//! yields decoded [`Osc133Event`]s, so it is unit-tested without a terminal. The
//! bridge owns spotting the sequences in the live PTY stream and driving the
//! state machine; it must NOT strip these bytes from `pty://output` (xterm is the
//! renderer and ignores them).
//!
//! ## Dogfood record (PRD 2.1, task #10 — the FINAL gate)
//!
//! Task #10 is the dogfood gate: prove the exec-state pipeline with a
//! deterministic synthetic path AND record real-shell behavior before the PRD is
//! considered done. Status of each leg, as of the phase-5 pass (2026-06-16):
//!
//! ### Automatically proven (deterministic, shell-free)
//!
//! Two synthetic e2e tests in `crate::bridge`'s test module drive crafted OSC 133
//! byte sequences through the PRODUCTION output pump
//! ([`crate::bridge::spawn_output_pump`]) with a synthetic mpsc receiver instead
//! of a live `Pty` — so they depend on NO real shell or local shell config:
//! - `synthetic_e2e_running_to_success_through_the_pump` — feeds `A/B` (inert),
//!   `C` (→ running), visible output, `D;0` (→ success); asserts the FULL chain:
//!   the `terminal://exec-state` emissions (running then success, keyed to the
//!   right `terminal_id`, unread semantics), the persisted DB row (success(0) +
//!   unread — the restart authority), AND that `pty://output` still carries the
//!   visible bytes (NO stripping). Dropping the tx → disconnect leaves the settled
//!   result untouched.
//! - `synthetic_e2e_running_to_error_and_normalize_on_exit_through_the_pump` —
//!   one terminal runs `C`→`D;3` (→ error(3)+unread); a second goes `C` with NO
//!   `D` then its PTY disconnects, proving normalize-on-exit settles the stale
//!   `running` to `idle` (no false badge), with per-terminal event routing.
//!
//! These compile + link clean (`cargo test --lib --no-run`). On THIS Windows host
//! the lib test HARNESS exe will not launch — every test, even the pure `osc7::`/
//! `osc133::` parser tests, aborts with `STATUS_ENTRYPOINT_NOT_FOUND`
//! (`0xc0000139`): a conpty (portable-pty) link-graph gap in the environment, NOT
//! a logic failure (a plain rustc binary launches fine here). The synthetic e2e
//! LOGIC was therefore additionally proven OUT-OF-BAND: `src-tauri/oob/`
//! `oob_synthetic_e2e.rs` is a standalone binary (no conpty deps, launches here)
//! that copies the OSC 133 parser + the `drive_exec_state`/normalize mapping +
//! the pump structure VERBATIM and runs the same scenarios. It passes all three
//! scenarios (success, error+normalize, and the real-PowerShell capture below).
//!
//! ### Real-shell dogfood
//!
//! - **PowerShell (Windows PowerShell 5.1, `powershell.exe`) — VALIDATED.**
//!   Dot-sourcing the EXACT [`crate::shellinteg`] `powershell_snippet()` and
//!   driving the wrapped `prompt` emitted, with NO visible pollution (stripping
//!   the OSC sequences leaves only `PS <cwd]> `):
//!   ```text
//!   after success ($?=True): <ESC>]133;D;0<BEL><ESC>]133;A<BEL>PS D:\Projects\nyx> <ESC>]133;B<BEL>
//!   after exit 3           : <ESC>]133;D;3<BEL><ESC>]133;A<BEL>PS D:\Projects\nyx> <ESC>]133;B<BEL>
//!   after exit 42          : <ESC>]133;D;42<BEL>...
//!   ```
//!   The exit code is recovered correctly (`$?`→0 else `$LASTEXITCODE`). A 131-byte
//!   stream of these REAL bytes (a success-then-error cycle) was then fed through
//!   the pump replica (oob scenario #3) and drove `running→success(0)` then
//!   `running→error(7)` exactly. `pwsh.exe` (PowerShell 7+) is NOT installed on
//!   this host, but the `prompt`-wrapper mechanism is edition-identical (5.1 vs 7),
//!   so 7 is covered by the same code path — a human should still tick it in a live
//!   window when pwsh is available.
//! - **bash / zsh — NOT INSTALLABLE/PRESENT on this Windows host** (no `bash`/`zsh`
//!   on PATH; a bare `bash` resolves to the WSL launcher). Their injection snippets
//!   are unit-tested for content (`crate::shellinteg` tests), but a live end-to-end
//!   dogfood requires a machine with real bash/zsh. See the manual steps below.
//!
//! ### Manual dogfood steps (for a human, in a LIVE nyx window)
//!
//! For EACH of bash, zsh, PowerShell (pwsh7 + Windows PowerShell), on a host where
//! that shell is installed:
//! 1. Launch nyx; open a terminal whose resolved shell (`$SHELL` / per-OS default,
//!    see [`crate::pty::resolve_shell`]) is the target shell. Confirm the prompt is
//!    YOUR normal prompt (the injection sources your real rc/profile first — no
//!    clobbering) and there is NO stray `133;`/escape text rendered anywhere.
//! 2. Run a command that SUCCEEDS (e.g. `true` / `echo ok`). While it runs, the
//!    sidebar row badge shows `running`; on exit it flips to `success`. If the
//!    terminal is NOT the active one, the success badge is UNREAD (a notification).
//! 3. Run a command that FAILS (e.g. `false` / `exit 3` in a subshell). Badge:
//!    `running` → `error` (unread when inactive).
//! 4. Select/view the terminal: the unread `success`/`error` badge marks READ
//!    (keeps its color/result, stops being a notification). Re-deselect: it must
//!    NOT pop back to unread.
//! 5. Run a long command and then CLOSE the terminal / kill the shell mid-command:
//!    the badge must NOT stay stuck on `running` — it normalizes to `idle` (or the
//!    last settled result), never a stale `running`.
//! 6. (Unsupported-shell honesty) Open a `cmd.exe` / `sh` / fish terminal: it stays
//!    `idle`-only — no false `running`, no fake success/error.

/// A decoded OSC 133 command-lifecycle event. Phase 3's state machine maps these
/// onto terminal exec-state; `A`/`B` are carried for completeness but are inert
/// for nyx (see the module mapping notes).
#[cfg_attr(not(test), allow(dead_code))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Osc133Event {
    /// `133;A` — prompt start. Inert for nyx.
    PromptStart,
    /// `133;B` — command start (end of prompt / input begins). Inert for nyx
    /// (we key "running" off `C`, not `B`; see module docs).
    CommandStart,
    /// `133;C` — pre-exec: the command is now running. → `running`.
    PreExec,
    /// `133;D;<exit>` — command end. `Some(code)` when a numeric exit status was
    /// present (`0` → success, non-zero → error); `None` when the `D` carried no
    /// parseable code (settle to a result, never a stale `running`).
    CommandEnd { exit_code: Option<i32> },
}

/// OSC introducer for the 133 (shell-integration) family: `ESC ] 133 ;`.
const INTRO: &[u8] = b"\x1b]133;";

/// Scan a raw PTY byte chunk and return every COMPLETE OSC 133 event it carries,
/// in order. A chunk can hold several (a full prompt cycle is `D;A` then `B`, and
/// a flood can pack many) — unlike OSC 7 (where only the latest cwd matters), the
/// exec-state machine needs each transition, so we return all of them.
///
/// Recognized framing mirrors [`crate::osc7`]: `ESC ] 133 ;` … terminator, where
/// the terminator is `BEL` (`0x07`) or `ST` (`ESC \`). An INCOMPLETE trailing
/// sequence (introducer seen but no terminator yet in this chunk) is left for the
/// next chunk — phase 2's pump carries a small tail buffer across chunk
/// boundaries so a split sequence is not lost.
#[cfg_attr(not(test), allow(dead_code))]
pub fn extract_events(chunk: &[u8]) -> Vec<Osc133Event> {
    let mut events = Vec::new();
    let mut search_from = 0;
    while let Some(rel) = find_subslice(&chunk[search_from..], INTRO) {
        let start = search_from + rel + INTRO.len();
        // Find the terminator: BEL or ST (ESC \).
        let mut end = None;
        let mut j = start;
        while j < chunk.len() {
            if chunk[j] == 0x07 {
                end = Some((j, j + 1));
                break;
            }
            if chunk[j] == 0x1b && j + 1 < chunk.len() && chunk[j + 1] == b'\\' {
                end = Some((j, j + 2));
                break;
            }
            j += 1;
        }
        match end {
            Some((payload_end, after)) => {
                let payload = &chunk[start..payload_end];
                if let Some(ev) = parse_payload(payload) {
                    events.push(ev);
                }
                search_from = after;
            }
            // Incomplete sequence (no terminator yet): stop; the pump retains the
            // tail and re-scans once more bytes arrive.
            None => break,
        }
    }
    events
}

/// Decode the inner payload of an OSC 133 sequence (the bytes between
/// `ESC ] 133 ;` and the terminator) into an [`Osc133Event`]. The payload is the
/// kind letter optionally followed by `;`-separated parameters (only `D` uses
/// one: the exit code). Unknown kinds yield `None` (robustly ignored).
fn parse_payload(payload: &[u8]) -> Option<Osc133Event> {
    let kind = *payload.first()?;
    match kind {
        b'A' => Some(Osc133Event::PromptStart),
        b'B' => Some(Osc133Event::CommandStart),
        b'C' => Some(Osc133Event::PreExec),
        b'D' => {
            // `D` or `D;<code>` (some shells emit extra `;key=val` params after
            // the code — VS Code does; we read only the first param as the code).
            let exit_code = parse_exit_code(&payload[1..]);
            Some(Osc133Event::CommandEnd { exit_code })
        }
        _ => None,
    }
}

/// Parse the exit code from a `D` payload's tail (everything after the `D`).
/// Shapes handled: `` (bare `D`) → `None`; `;0` → `Some(0)`; `;137` → `Some(137)`;
/// `;0;cmd_duration=12` → `Some(0)` (extra params ignored); `;` or `;xx`
/// (non-numeric) → `None`. Never panics on malformed input.
fn parse_exit_code(tail: &[u8]) -> Option<i32> {
    // Strip the leading `;` that separates the kind from the first param.
    let rest = tail.strip_prefix(b";")?;
    // The code runs until the next `;` (if a shell appended more params).
    let code_bytes = match rest.iter().position(|&b| b == b';') {
        Some(idx) => &rest[..idx],
        None => rest,
    };
    if code_bytes.is_empty() {
        return None;
    }
    std::str::from_utf8(code_bytes)
        .ok()?
        .trim()
        .parse::<i32>()
        .ok()
}

/// First index of `needle` in `haystack`, or `None`. Allocation-free; mirrors the
/// helper in [`crate::osc7`].
fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    haystack.windows(needle.len()).position(|w| w == needle)
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- Per-marker decoding ------------------------------------------------

    #[test]
    fn parses_prompt_and_command_start_and_preexec() {
        assert_eq!(
            extract_events(b"\x1b]133;A\x07"),
            vec![Osc133Event::PromptStart]
        );
        assert_eq!(
            extract_events(b"\x1b]133;B\x07"),
            vec![Osc133Event::CommandStart]
        );
        assert_eq!(
            extract_events(b"\x1b]133;C\x07"),
            vec![Osc133Event::PreExec]
        );
    }

    #[test]
    fn command_end_exit_zero_is_success_code() {
        // Empirically the bash probe emitted exactly this after `true`.
        assert_eq!(
            extract_events(b"\x1b]133;D;0\x07"),
            vec![Osc133Event::CommandEnd { exit_code: Some(0) }]
        );
    }

    #[test]
    fn command_end_nonzero_exit_is_carried() {
        // The PowerShell probe emitted `D;3` after `cmd /c exit 3`; bash emitted
        // `D;1` after `false`. Non-zero codes round-trip as-is.
        assert_eq!(
            extract_events(b"\x1b]133;D;3\x07"),
            vec![Osc133Event::CommandEnd { exit_code: Some(3) }]
        );
        assert_eq!(
            extract_events(b"\x1b]133;D;1\x07"),
            vec![Osc133Event::CommandEnd { exit_code: Some(1) }]
        );
        // A larger, signal-style code (e.g. 137 = 128+SIGKILL) still parses.
        assert_eq!(
            extract_events(b"\x1b]133;D;137\x07"),
            vec![Osc133Event::CommandEnd {
                exit_code: Some(137)
            }]
        );
    }

    #[test]
    fn command_end_missing_code_is_none_not_running() {
        // A bare `D` (no exit param) must settle to a result with no code — never
        // leave the terminal stuck in `running`.
        assert_eq!(
            extract_events(b"\x1b]133;D\x07"),
            vec![Osc133Event::CommandEnd { exit_code: None }]
        );
        // `D;` (empty param) and `D;xx` (garbage) likewise yield None, not a panic.
        assert_eq!(
            extract_events(b"\x1b]133;D;\x07"),
            vec![Osc133Event::CommandEnd { exit_code: None }]
        );
        assert_eq!(
            extract_events(b"\x1b]133;D;oops\x07"),
            vec![Osc133Event::CommandEnd { exit_code: None }]
        );
    }

    #[test]
    fn command_end_ignores_extra_params_after_code() {
        // Some integrations append `;key=val` after the code (e.g. VS Code). We
        // read only the first param as the exit code.
        assert_eq!(
            extract_events(b"\x1b]133;D;0;cmd_duration=12\x07"),
            vec![Osc133Event::CommandEnd { exit_code: Some(0) }]
        );
    }

    // --- Terminators (BEL and ST), mirroring OSC 7 ---------------------------

    #[test]
    fn accepts_st_terminator_not_just_bel() {
        // `ST` = ESC \ . The bridge must accept both, exactly like OSC 7.
        assert_eq!(
            extract_events(b"\x1b]133;D;0\x1b\\rest"),
            vec![Osc133Event::CommandEnd { exit_code: Some(0) }]
        );
        assert_eq!(
            extract_events(b"\x1b]133;C\x1b\\"),
            vec![Osc133Event::PreExec]
        );
    }

    // --- Realistic streams ---------------------------------------------------

    #[test]
    fn parses_a_full_prompt_cycle_in_order() {
        // A real prompt cycle interleaved with rendered prompt text + output.
        // C (running) → D;0 (success) → A → B for the next prompt.
        let chunk = b"\x1b]133;C\x07hello\r\n\x1b]133;D;0\x07\x1b]133;A\x07PS C:\\> \x1b]133;B\x07";
        assert_eq!(
            extract_events(chunk),
            vec![
                Osc133Event::PreExec,
                Osc133Event::CommandEnd { exit_code: Some(0) },
                Osc133Event::PromptStart,
                Osc133Event::CommandStart,
            ]
        );
    }

    #[test]
    fn returns_every_event_when_several_are_packed() {
        // Two finished commands coalesced into one pump chunk: BOTH ends surface
        // (OSC 7 keeps only the last cwd; exec-state needs each transition).
        let chunk = b"\x1b]133;D;0\x07out\x1b]133;C\x07\x1b]133;D;2\x07";
        assert_eq!(
            extract_events(chunk),
            vec![
                Osc133Event::CommandEnd { exit_code: Some(0) },
                Osc133Event::PreExec,
                Osc133Event::CommandEnd { exit_code: Some(2) },
            ]
        );
    }

    // --- Robustness: irrelevant sequences, splits, no false positives --------

    #[test]
    fn ignores_unrelated_osc_and_plain_output() {
        // An OSC 7 cwd sequence and ordinary text carry no 133 events.
        assert!(extract_events(b"\x1b]7;file:///home/kris\x07").is_empty());
        assert!(extract_events(b"just some output\r\n$ ").is_empty());
        // An OSC 133 with an unknown kind letter is robustly ignored, not panicked.
        assert!(extract_events(b"\x1b]133;Z\x07").is_empty());
    }

    #[test]
    fn incomplete_trailing_sequence_is_left_for_next_chunk() {
        // Introducer present but no terminator yet: we emit what completed before
        // it and stop (the pump retains the tail and re-scans on the next chunk).
        let chunk = b"\x1b]133;C\x07\x1b]133;D;0"; // D never terminated in this chunk
        assert_eq!(extract_events(chunk), vec![Osc133Event::PreExec]);
        // A lone, unterminated introducer yields nothing.
        assert!(extract_events(b"\x1b]133;D;0").is_empty());
    }

    #[test]
    fn split_sequence_recovered_once_reassembled() {
        // Simulates the pump stitching a chunk boundary: the first half yields
        // nothing; concatenated with the tail it decodes. (Phase 2 carries the
        // tail buffer; this asserts the parser is position-independent.)
        let head = b"\x1b]133;D;".to_vec();
        assert!(extract_events(&head).is_empty());
        let mut whole = head;
        whole.extend_from_slice(b"42\x07");
        assert_eq!(
            extract_events(&whole),
            vec![Osc133Event::CommandEnd {
                exit_code: Some(42)
            }]
        );
    }

    #[test]
    fn empty_chunk_yields_no_events() {
        assert!(extract_events(b"").is_empty());
    }
}
