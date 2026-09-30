# Trellis standard configuration and flat-only audit

**Snapshot:** 2026-09-30. **Result:** the bounded standard-name migration and
legacy appearance cleanup are present in the reviewed source. The focused
configuration and cross-regression tests pass. This is not full RISC OS
`*Configure` compatibility or visual sign-off.
The follow-up `trellis-configuration-corrections-audit.md` supersedes this
batch's earlier output-profile option and records the six-key v3 contract,
inline `IF ... THEN PROC` continuation fix, and current end-to-end evidence.

## Contract crosswalk

The PRM defines generic `*Configure [option [value]]`: no arguments list the
available options, some options accept several or no values, and numeric
parameters may be decimal, `&`-prefixed hexadecimal, or `base_num` (base 2–36).
`*Configure WimpMode screen_mode|Auto` configures the mode for power-on/reset
and desktop entry/exit; `Mode` names the same setting on RISC OS 3+, while RISC
OS 2 differs. The PRM video chapter defines textual mode selectors with three-
or four-digit X/Y fields, C/G depth tokens, and optional EX/EY/frame-rate
selectors. See the [PRM Configure reference](https://www.riscos.com/support/developers/prm/memoryman.html),
[WimpMode reference](https://www.riscos.com/support/developers/prm/wimp.html),
and [video mode-string reference](https://www.riscos.com/support/developers/prm/video.html).

| Current option | Relationship to RISC OS | Actual hosted policy and limits |
|---|---|---|
| `Language` | Standard option name and numeric spelling. | Accepts decimal, `&hex`, and `base_num`, but only values 0 (MOS prompt) and 3 (desktop). Other module numbers are deliberately rejected; value 0 remains the default. |
| `WimpMode` / `Mode` | Standard configured-mode names/alias. | Accepts `Auto` or exactly `X<width> Y<height> C/G<depth>`. Six logical sizes and C2/C16/C256/C32K/C16M/G4/G16/G256 are supported; three/four digit fields, including zero-padded values, canonicalize. `Auto` follows host content size, not a sensed monitor lead. Physical mode IDs, C4/C64/C32T, EX/EY and frame-rate selectors are unavailable because Trellis has no monitor/mode table. |
| `BASICMode`, `BASICProfile`, `BASICTarget`, `BASICEngine` | Trellis-specific execution preferences. | Retained unchanged. `BASICProfile` is one allowed ASCII name up to 232 bytes so it fits the bounded command/read path. |
| `DEFAULTS` | Project extension, not a PRM option. | Restores the explicit six-key v3 default table. No-argument `*CONFIGURE` lists supported names; `*STATUS [option]` displays one or all. |

This is a bounded dialect, not the generic PRM option system: it does not
implement arbitrary option arities, monitor-selected numeric modes or all
video selectors. Historical `DisplayResolution` and `DisplayColour` are not
claimed as standard option names.

## Legacy migration and Flat-only result

The persisted-file reader accepts previous v1 rows only as migration input.
Old fixed resolution/palette pairs become the corresponding single
`WimpMode` selector; old `Window` resolution becomes
`Auto` with the full-colour C16M/Rgb888 default. Any old `WindowFurniture`
value is ignored. Previous v2 files may contain `TrellisOutputProfile`; an
explicit `WimpMode` wins and the profile row is discarded. A read-only
load/`STATUS` does not rewrite the source file; the next successful setting
write or `DEFAULTS` writes canonical v3 data with six public keys and no
obsolete rows. Public `*CONFIGURE`/`*STATUS` reject all retired names, and the
complete replacement/reset parser rejects them too.
The old Bevelled v1 fixture reaches the Language-3 desktop startup before a
write, and both Bevelled and Flat fixtures lose their obsolete row on a
successful write.

The active desktop code has one flat rendering path: the appearance preference
and bevelled-only `Tools3d,ff9` asset/path are removed. Remaining
`WindowFurnitureLayout` types and helper names describe geometry and
hit-testing; they do not select a style. References to the old setting remain
only where expected for migration, negative tests, provenance, and the
documented removal. I found no live Bevelled renderer branch or resource
selection in `src/` or `modules/`.

The save boundary is unchanged: BASIC64 owns option names, values, validation,
defaults, diagnostics and presentation; Rust provides checked caller-memory
transfer, typed store validation and atomic persistence. Configuration writes
still require the original Task's `ConfigurationWrite` authority. Public
`STATUS` remains read-only; denied callers and invalid input do not change the
saved file. This pass confirms that ordinary/source-only/module-only callers
remain denied and trusted MOS/configuration-manager callers retain their
separate write routes.

## Evidence and remaining limits

Independent focused checks on the reviewed snapshot:

- `ACORN_CONFIG_PATH=/private/tmp/<unique>.configure cargo test --no-default-features --test trellis_standard_configuration -- --nocapture` — 1 passed on the prior standard-setting batch. The correction batch replaces the earlier seven-key contract with six-key v3 status/defaults, WimpMode-only resolution/palette selection, v1/v2 migration precedence, Auto/full-colour behavior, retired-key rejection, and persistence; see the correction audit for fresh evidence.
- `ACORN_CONFIG_PATH=/private/tmp/<unique>.configure cargo test --no-default-features --test trellis_mos_configuration trellis_configure_status_persist_and_enforce_task_scoped_write_authority -- --exact --nocapture` — 1 passed. Covers authority, scratch preservation, persistence/denial and the retained `*C.` abbreviation.
- `ACORN_CONFIG_PATH=/private/tmp/<unique>.configure cargo test --no-default-features --lib configure_` — 3 passed, including Rust configuration validation and the Configure abbreviation unit test.
- `cargo fmt --all -- --check` and `git diff --check` — passed.

The `*C.`-before-Catalogue behavior was initially absent in the new BASIC64
resolver and caused the old MOS integration to fail; it was restored and the
regression then passed. No in-scope defect remains in the final reviewed
snapshot. The tests verify removal of the style switch and the source contains
only the flat branch, but there is no public scene/pixel assertion or live GPU
screenshot proving visual output; do not treat this as visual acceptance.

Broader generic `*Configure` compatibility and hardware mode selection remain
out of scope. BASIC64's explicit default table and Rust's defensive typed
schema/defaults are still duplicate descriptions and need to stay covered by
drift tests. The malformed-persisted-file recovery gap noted when this audit
was written is superseded by `trellis-configuration-recovery-audit.md`: boot
now latches a recovery cause, uses safe defaults without changing the file,
and allows an authorized repair after the prompt is available.
