# Trellis MOS configuration audit

**Snapshot:** 2026-09-30. **Result:** the bounded `*CONFIGURE`/`*STATUS`
migration is implemented in BASIC64 and passed the focused public integration
test on the reviewed snapshot. WP5.3 remains partial: other MOS command families
still use the transitional Rust adapter, and this is not UI or full CLI parity.
The follow-up `trellis-configuration-corrections-audit.md` supersedes this
audit's original seven-key schema: its six-key v3 WimpMode-only contract and
inline-dispatch regression evidence describe current behavior. This file
retains the original authorization and persistence review.

## Contract and deliberate scope

The RISC OS PRM defines `*Configure [option [value]]`; without parameters it
lists available settings, while individual options may take multiple values or
none, and take effect immediately or after reset depending on the option.
`*Status [option]` reports one setting or all settings. The hosted profile is a
deliberately smaller project contract: six named preferences stored in a
per-user file (or `ACORN_CONFIG_PATH`), not CMOS; `Language` accepts standard
numeric spellings but only module IDs 0 and 3; `WimpMode`/`Mode` implements a
bounded, standard-style composite selector. `*CONFIGURE DEFAULTS` is a project
extension. These are not claims of full PRM option coverage. See the [RISC OS
PRM configuration reference](https://www.riscos.com/support/developers/prm/memoryman.html),
[WimpMode](https://www.riscos.com/support/developers/prm/wimp.html), and
[video mode-string reference](https://www.riscos.com/support/developers/prm/video.html).

The current v3 keys/defaults are `Language=0`, `BASICMode=AUTO`,
`BASICProfile=AUTO`, `BASICTarget=AUTO`, `BASICEngine=INTERPRETER`, and
`WimpMode=AUTO`. `Language` accepts decimal, `&hex`, and `base_num`, then
canonicalizes to `0` or `3`. `WimpMode` (also `Mode`) accepts `Auto` or
`X<width> Y<height> C/G<depth>`; dimensions must be one of 640x480, 800x600,
1024x768, 1152x864, 1280x1024, or 1600x1200, with C2/C16/C256/C32K/C16M/G4/G16/G256.
Three- or four-digit X/Y numbers are accepted and canonicalized. Monitor mode
numbers, EX/EY scaling, and frame-rate selectors are unsupported because no
monitor/driver mode table is hosted. WimpMode is the only public resolution
and palette setting: fixed selectors choose both, while Auto uses host size
and full-colour C16M/Rgb888.

Legacy configuration-file keys `DisplayResolution`, `DisplayColour`,
`TrellisOutputProfile`, and `WindowFurniture` are migration-only. Old fixed
resolution/color pairs map to the corresponding WimpMode selector; Window
maps to Auto/full-colour regardless of old color. A v2 WimpMode selector wins
over the retired profile row. Any old furniture value is discarded. Loading
alone does not rewrite the file; the next successful setting write or DEFAULTS
saves the canonical six-key v3 form. BASIC64 commands and full replacement
reject the old keys. DEFAULTS is `Auto` for WimpMode and the other Auto
settings, and `Language=0`.

The PRM `OS_CLI` contract accepts a NUL-, LF-, or Return-terminated command,
preserves R0, and caps a command at 256 bytes including its terminator. The
current BASIC64 owner implements those bounds. RISC OS command abbreviations
are resolved by first match among registered commands; the user guide notes
that the minimum abbreviation can change as commands are added. This hosted
dispatcher instead preserves its established fixed order: `*C.` selects
Configure before Catalogue, `*CA.` selects Catalogue, `*CONF.` selects
Configure, and `*S.` selects Status. That is deterministic project behavior,
not RISC OS's dynamic module/alias resolution. See the [PRM CLI
entry](https://www.riscos.com/support/developers/prm/cli.html) and [RISC OS 3
command-line guide](https://www.riscos.com/support/users/userguide3/book3b/book3_2.html).

## Ownership and coverage

| Route | BASIC64 policy | Rust mechanism / boundary | Audit evidence and remaining limit |
|---|---|---|---|
| `OS_CLI` (`&05`) | `TrellisCommands.bas64` owns recognition, final-dot abbreviation, argument parsing, values/defaults, diagnostics and output for `*CONFIGURE`/`*CONF.`, `*STATUS`, read-only `*INSPECT`, and the documented classic module-command subset. | Registry dispatch invokes the manifest-owned definition. Checked task-local command scratch is released by the dispatcher on both success and error; non-migrated commands go through `Host.MOS.ExecuteLegacy`. | Public config integration covers whitespace/case, abbreviations and the `*C.`/`*CA.` precedence; 256-byte command handling/R0 behavior is also covered in the CLI tests. `tests/trellis_classic_module_commands.rs` covers the current Inspect/classic routes. The obsolete `*TRELLIS` namespace is no longer recognized. Remaining MOS command semantics are still Rust fallback. |
| `*CONFIGURE option value` | BASIC64 accepts one option and its bounded value, including the three-token WimpMode selector; owns option aliases, supported values, defaults, diagnostics, and output. Six keys: `Language`; `WimpMode`/`Mode`; `BASICMode`; `BASICProfile`; `BASICTarget`; `BASICEngine`. | `Host.Configuration.WriteValue` performs checked logical-memory reads, task-right enforcement and typed persistence. The Rust store defensively validates the schema and atomically saves through a sibling temporary file and rename. | Integration covers the PRM Language numeric spellings, WimpMode names/forms/depths, Auto/full-colour semantics, BASIC values, the 232/233-byte profile limit, rejected retired keys, and unchanged data after validation or persistence failure. Full PRM option arities/quoting and hardware mode-number selection remain unsupported. |
| `*CONFIGURE DEFAULTS` | BASIC64 owns the explicit six-key v3 reset payload and success/error presentation. Defaults are `Language=0`, `BASICMode=AUTO`, `BASICProfile=AUTO`, `BASICTarget=AUTO`, `BASICEngine=INTERPRETER`, and `WimpMode=AUTO` (host-sized, full-colour C16M/Rgb888). | `Host.Configuration.ReplaceAll` checks authority and caller memory, requires exactly the current schema, rejects migration-only keys, and persists atomically. | Public correction test compares no-file Status output with post-reset output and Rust typed defaults. The BASIC64 table and Rust defensive schema/defaults remain duplicated and need drift coverage when settings change. |
| `*STATUS [option]` | BASIC64 checks arity/name and formats one or all six values, using canonical WimpMode/Language forms. | `Host.Configuration.ReadValue` reads typed effective settings into bounded caller memory and returns a recovery-kind code; absent files are ordinary defaults, while latched damaged files expose safe defaults plus their path-redacted category. Status is explicitly public-read; it does not require the caller's `ConfigurationWrite` right. | Ordinary, source-only, module-only and config-only tasks can read. Read-only calls preserve caller memory outside their checked scratch area. Rust still owns the typed `status_value` field-to-string mapping as part of its storage adapter; it is not a Rust CLI handler, but it is another drift point. The full malformed/unreadable/oversized matrix is in `tests/trellis_configuration_recovery.rs`. |
| Startup `Language` | `Boot.bas64` chooses MOS prompt (0) or desktop (3) and requests the host handoff; runtime consumes that request rather than interpreting the preference. | `System.ReadStartupLanguage` reaches a protected typed read primitive. | Focused tests exercise both values and verify Language is independent of BASICEngine. The later `trellis-configuration-recovery-audit.md` supersedes the original malformed-file gap: invalid stored settings now yield safe defaults and a path-redacted BASIC64 diagnostic instead of failing Boot.Start. |
| `ACORN_DISPLAY APPLY` | Not a `*CONFIGURE` handler; this separate custom display service remains a Rust-owned public SWI. | It requires `ConfigurationWrite`, persists before changing live Wimp settings, and reports save failure without applying. Wimp application is crate-private. The same invariant rejects Window/Auto with a non-Rgb888 palette using normal completion and R8=1 before either persisted or live state changes. | Integration verifies ordinary denial leaves file and live display unchanged, trusted MOS can apply before desktop handoff, and Auto+BW is rejected without mutation. This alternate writer is explicitly gated but remains outside the BASIC64 configuration command migration. |

## Authority and persistence findings

`Task::new` and `Runtime::desktop_task` are ordinary. The interactive runtime
uses the explicit `Task::trusted_mos_session` constructor; a separate
configuration-manager profile exists for host use. Rights are private to a
Task object, not inferred from its public numeric ID. Reconstructing an ordinary
Task with an authorized task's ID does not grant access. TrellisCommands has a
provider `ConfigurationStoreWrite` grant, but that grant does not confer caller
authority: `configuration_write_value`, `configuration_replace_all`, and
`ACORN_DISPLAY APPLY` check the original Task. The service checks the error
buffer shape, then denies before reading write payloads or mutating storage;
guest imports of the protected writer capability are rejected. Nested guest
`OS_CLI` calls preserve the caller Task, so an unprivileged wrapper cannot use
the privileged provider as a deputy.

The public integration verified denied ordinary/source-only/module-only and
same-ID spoof writes, authorized config-only/MOS writes, nested calls, denied
guest capability import, unchanged file state on denial and forced persistence
failure, task-local scratch cleanup and preservation of a sentinel memory
range, and independent fresh-dispatcher reads. The host's test-only trusted
configuration-manager profile is not an end-user grant UI. Trusted MOS code
and programs run within that same Task share its authority; there is no
per-program sandbox. Host revocation is task teardown/replacement, not a guest
revocation API.

## Defects, deviations, and remaining tests

No in-scope `*CONFIGURE`/`*STATUS` defect remained in the final reviewed
snapshot. During review, the focused test exposed fixed-address scratch writes,
argument-type mismatches in the shared CLI module-query route, and an
unavailable pre-handoff Wimp reference for trusted `ACORN_DISPLAY APPLY`; these
were corrected before the final run. Rust no longer contains Configure/Status
command branches; `src/swi.rs` retains only generic help text plus typed
configuration/storage mechanisms.

At the time of this audit, an invalid stored configuration could fail
`Boot.Start` before the CLI was available. That finding is retained as history,
not current behavior: `trellis-configuration-recovery-audit.md` and
`tests/trellis_configuration_recovery.rs` cover the later recovery path. The
typed store bounds reads to 64 KiB, defaults safely without rewriting the
source, and preserves original bytes in a sibling copy during the first
authorized successful repair. This remains separate from native capsule
recovery, and no public recovery SWI is exposed before foundation startup.

Other remaining work is deliberate scope: move the other legacy MOS commands,
decide whether to migrate the separate `ACORN_DISPLAY` service, expand the
hosted option set only through a documented contract, and keep BASIC64/Rust
schema/default mirrors synchronized. Full WP5.3 completion and browser/UI parity
are not claimed.

Verification on this snapshot:

- `ACORN_CONFIG_PATH=/private/tmp/acorn-trellis-mos-configuration-audit-final-<unique>.configure cargo test --no-default-features --test trellis_mos_configuration trellis_configure_status_persist_and_enforce_task_scoped_write_authority -- --exact --nocapture` — 1 passed.
- `ACORN_CONFIG_PATH=/private/tmp/acorn-trellis-config-audit-unit2-<unique>.configure cargo test --no-default-features --lib configure_accepts_supported_values_and_conf_abbreviation -- --nocapture` — 1 passed.
- `rustfmt --edition 2024 --check tests/trellis_mos_configuration.rs` and `git diff --check` on the reviewed implementation/docs/test paths — passed. The full workspace suite was not run for this audit.
