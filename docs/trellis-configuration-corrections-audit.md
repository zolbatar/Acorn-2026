# Trellis configuration corrections audit

**Snapshot:** 2026-09-30. **Result:** the command fallthrough is reproduced on
the prior snapshot and fixed in the current interpreter; `WimpMode` is now the
single public persisted resolution/palette setting. The focused correction
integration and independent stdio reproduction pass. This is not full BBC
BASIC control-flow compatibility or visual sign-off.

This follow-up supersedes the earlier “no in-scope defect remains” conclusion
in `trellis-standard-configuration-audit.md`: that pass accepted expected
substrings and missed extra diagnostics emitted after successful commands.

## Command fallthrough and BASIC control flow

Before the fix, `TrellisCommands.bas64` used inline branch lists such as
`IF CLI_COMMAND$ = "CONFIGURE" THEN PROC ConfigureCommand: ENDPROC`. The
parser represents colon-separated inline statements as the selected IF body.
Calling `PROC ConfigureCommand` jumps to its body; when it returned, the
interpreter resumed after the call and skipped the sibling `ENDPROC`. Dispatch
then fell through to `ExplainUnsupportedModuleCommand` and, for unmatched
routes, the legacy command handler. This affected both CONFIGURE and STATUS;
removing or hiding the extra message would not have fixed the dispatch.

The official BBC BASIC reference says `ENDPROC` passes control to the
statement after the calling `PROC`, and documents inline
`IF a<=0 THEN ENDPROC ELSE PROCrecurse(a-1)` as valid syntax. The implementation
now suspends the remaining inline statements on the procedure return frame,
restores them after return, and preserves nested-IF order. GOTO and ENDPROC
remain control transfers rather than accidentally resuming canceled siblings.
The fix is in the shared interpreter, not a CONFIGURE-only special case.
[BBC BASIC ENDPROC reference](https://www.riscos.com/support/developers/bbcbasic/bbcref.html)

The regression fixture checks a PROC followed by a second PROC and GOTO (the
GOTO skips the later sibling), an early inline ENDPROC (skipping its siblings),
and nested inline IF call ordering (`312`). The public test also compares
complete command output, so appended `Unsupported`/`Bad command` text cannot
pass as a harmless substring.

## One display setting and historical boundary

The PRM defines `*Configure WimpMode screen_mode|Auto`; `*Configure Mode` is
an alias for the same configured value on later RISC OS versions. Historical
`Auto` senses the monitor lead, falling back to mode 12 where sensing is not
available. PRM mode strings allow three/four digit X/Y values, C/G depth
selectors, and optional EX/EY and frame-rate fields. The hosted implementation
uses those names and a bounded textual-selector shape, but does not claim the
hardware behavior or the entire selector grammar.

| Setting/path | Verified current contract | Deliberate hosted boundary |
|---|---|---|
| `*CONFIGURE WimpMode` / `Mode` | One v3 public option selects both resolution and palette. Fixed modes cover six logical sizes and C2/C16/C256/C32K/C16M/G4/G16/G256. | `Auto` means host-content size plus full-colour C16M/Rgb888, not monitor-lead sensing. No monitor mode table, numeric mode IDs, EX/EY, frame-rate, C4/C64/C32T, or other unsupported renderer combinations. |
| `ACORN_DISPLAY APPLY` | The SWI's separate resolution/colour registers are translated into the same WimpMode setting. Fixed selections persist together. `Window` plus a non-Rgb888 palette returns R8=1. | This remains a hosted service ABI, not a separate public Configure option or a claim of historical mode selection. |
| `TrellisOutputProfile` | No longer in BASIC64 help, command validation, STATUS, defaults, or v3 serialization. | A v2 row is decoded only for migration and discarded; explicit v2 WimpMode wins. |

`BasicConfiguration` keeps `DisplaySettings` as an internal renderer-facing
projection, but every supported setter derives it from `WimpMode`; the old
separate profile is not a second setting. The configuration store rejects
Auto/non-full-colour before changing its in-memory value or saving. Wimp apply
persists first and only then changes live settings, so a save or validation
failure leaves live display state unchanged.

## Migration, permissions, and Flat-only behavior

The saved v3/public table has six keys: `Language`, `WimpMode`, `BASICMode`,
`BASICProfile`, `BASICTarget`, and `BASICEngine`. Old v1 `DisplayResolution` /
`DisplayColour` fixed pairs migrate into one WimpMode selector; `Window` maps
to Auto/full-colour. Old v2 `TrellisOutputProfile` is discarded, including
when paired with Auto; WimpMode remains authoritative. `WindowFurniture` is
ignored during file migration. These names are rejected by current public
configuration writes and the complete DEFAULTS replacement payload. A
read-only STATUS/boot does not rewrite the old file; the next authorized save
writes canonical v3 rows. Legacy default serialization uses uppercase enum
values (`AUTO`, `INTERPRETER`).

The integration fixture starts actual `Runtime::run()` with v1 fixed-display
data and v2 Auto plus retired BW profile, verifies the resulting live Wimp
resolution/colour, and checks the source file remains byte-for-byte unchanged
until a successful save. It then checks six-key v3 output with no retired
rows. The ACORN_DISPLAY Auto+BW case verifies R8=1 and byte-for-byte saved
configuration plus live settings rollback.

`ConfigurationWrite` is still checked at the Rust configuration write/reset
mechanism and at `ACORN_DISPLAY APPLY`; a task ID alone does not grant it. The
correction test verifies ordinary denial and trusted-MOS success, and the
separate task-authority integration still passes its ordinary/source-only/
manager/configuration-profile matrix. Read-only STATUS remains public.

The active renderer/resource search found no Bevelled selection branch or
`Tools3d,ff9` use. `WindowFurniture` remains only as ignored migration input
and in negative/migration tests or documentation; `WindowFurnitureLayout`
names geometry/hit-testing, not a style. The bevel-only sprite was removed.
Glossy icon descriptions in the branding directory concern icons, not the
window-furniture renderer.
This establishes a single flat code path, not pixel-level visual acceptance:
there is no public scene/pixel assertion or live screenshot in this test lane.

## Evidence and remaining limits

Independent checks on this snapshot:

- `cargo test --no-default-features --lib inline_if_resumes_after_procedure_call_and_honours_endproc -- --nocapture` — 1 passed, including flat and nested continuation ordering, GOTO, and early ENDPROC.
- `cargo test --features experimental-jit --lib inline_if_resumes_after_procedure_call_and_honours_endproc -- --nocapture` — 1 passed. The fixture runs an interpreter instance; this verifies the feature build and interpreter behavior, not native optimized-procedure continuation.
- `ACORN_CONFIG_PATH=/private/tmp/acorn-config-correction-audit-final-20260930-03.configure cargo test --no-default-features --test trellis_configuration_corrections -- --nocapture` — 1 passed on the final snapshot. An earlier audit run caught only stale mixed-case expected serialization; the fixture was aligned with the canonical uppercase store before the passing run.
- `ACORN_CONFIG_PATH=/private/tmp/acorn-config-correction-auth-audit-20260930-01.configure cargo test --no-default-features --test trellis_authorization trellis_services_enforce_task_scoped_read_and_management_authority -- --exact --nocapture` — 1 passed.
- `printf '*CONFIGURE\r*CONFIGURE Language 3\r*STATUS Language\r*INSPECT\r*Modules\rQUIT\r' | env ACORN_CONFIG_PATH=/private/tmp/acorn-config-correction-stdio-final-20260930-03.configure ACORN_DEMO_VOLUME=/Users/daryl/GitHub/Acorn-2026/demo-volume cargo run --no-default-features -- --stdio` — exit 0; each migrated route rendered once, with no trailing `Unsupported *CONFIGURE`, `Unsupported *STATUS`, or `Bad command` output.
- `git diff --check` — passed.
- `ACORN_CONFIG_PATH=/private/tmp/acorn-configuration-corrections-audit-lib-20260930-01.configure cargo test --no-default-features --lib` — 179 passed, 1 failed, 2 ignored. The sole failure is the unrelated dirty branding asset dimension assertion (actual 541×853, expected 556×872).

The earlier substring-only config test did not expose the fallthrough; the new
whole-output/sequential test does. Remaining work is broader BBC BASIC inline
control-flow corpus coverage (including errors and GOSUB), an acceptance test
that actually installs/runs optimized JIT procedures with suspended inline
continuations, and pixel/screenshot confirmation of Flat-only rendering. The
malformed-configuration startup gap noted in this snapshot is superseded by
`trellis-configuration-recovery-audit.md` and its boot/status/repair tests.

Primary contracts: [PRM `*Configure WimpMode`](https://www.riscos.com/support/developers/prm/wimp.html),
[PRM video mode strings](https://www.riscos.com/support/developers/prm/video.html),
and [BBC BASIC ENDPROC](https://www.riscos.com/support/developers/bbcbasic/bbcref.html).
