# ColourTrans named-service ownership audit

This audit covers the three existing name-only services
`ColourTrans_ConvertHSVToRGB`, `ColourTrans_SetGCOL`, and
`ColourTrans_WritePalette`. It does not claim native numeric ColourTrans
compatibility, complete palette support, completion of WP5.5, or completion of
Phase 5.

## Ownership and dispatch

`modules/ColourTrans.bas64` exports the three exact service names as BASIC64
procedures. They remain named-only: the active module registry has no numeric
SWI identity for any of them. `dispatch_named_swi` recognizes this fixed
name-only family, checks that the active `ColourTrans` module exports the
requested symbol, and invokes its retained BASIC64 definition. Rust's role
here is the bounded name-to-definition bridge and register contract, not the
colour-conversion or palette policy. Quiesced calls fail as inactive-owner
errors; after retirement, calls fail because there is no registered owner.
There is no inline Rust conversion, GCOL mutation, or palette no-op fallback.
The `X` form uses the common task-scoped error-block convention.

`COLOURTRANS_SETGCOL` imports only `Host.Graphics.SetPackedRgb` under the
`GraphicsRaster` capability. That mechanism resolves the invoking Task's
selected graphics context using the same selector as Plot and ReadPoint: the
solo/non-Wimp shell uses its shared display raster; Wimp/desktop mode uses the
configured display surface, a bounded task-default surface for other
no-window tasks, or the active redraw window surface. It changes the packed
hosted GCOL state without altering the supplied registers. It does not grant
filesystem, task-management, or display-configuration authority. The
conversion and no-op services require no host primitive.

The ColourTrans integration fixture proves SetGCOL's packed color appears in
the snapshot consumed by the caller's subsequent Plot and is visible through
the same caller's ReadPoint path. The internal Wimp redraw test also calls
name-only SetGCOL while each redraw window context is active, checks that
window's packed graphics color, restores the indexed GCOL, and then exercises
Plot/ReadPoint before verifying context restoration.

## Compatibility boundary

The primary source is the [RISC OS PRM Volume 3, Chapter 60: ColourTrans](https://www.riscos.com/support/developers/prm/colourtrans.html).
It defines `ColourTrans_ConvertHSVToRGB` as numeric SWI `&40759`, with all
three inputs in 16.16 form (hue 0–360, saturation/value 0–1) and byte RGB
outputs in R0–R2. The PRM also specifies an error when both hue and saturation
are zero. The hosted name-only adapter instead preserves its pre-existing
contract: signed 16.16 hue, R1 saturation divided by 65280, R2's low byte as
value divided by 255, and rounded byte RGB in R0–R2. Hue wraps into the hosted
range; saturation is capped at 1. A zero hue and zero saturation returns the
corresponding grey value rather than the native error.

Native `ColourTrans_SetGCOL` is numeric `&40743`: R0 is a palette entry,
R3 carries foreground/background and ECF flags, R4 carries a GCOL action,
and outputs include the selected GCOL and mode information. The hosted
name-only adapter intentionally retains the sample-facing behavior instead:
R0 is a packed `&BBGGRR00` word, the current graphics foreground state is set,
and all registers are preserved. The hosted raster's indexed-mode projection
does not make this a native palette lookup or exact indexed-colour contract.

Native `ColourTrans_WritePalette` is numeric `&4075D` and writes a current or
sprite palette through guest pointers and flags. The hosted name-only service
does not dereference those native pointer-shaped registers, does not change
palette state, and returns successfully with registers unchanged. This is an
explicit compatibility shim, not palette-write support.

These differences are why the module publishes no numeric aliases for the
three names. Mapping a native numeric number to the existing hosted ABI would
silently advertise incompatible register semantics. The module's name-only
exports use the existing BASIC64 symbol-export table and are dispatched only
for this explicit service family; they do not fabricate a numeric identity.

## Evidence and remaining limits

Independent checks on the current shared tree:

- `RICOCHET_CONFIG_PATH=/tmp/ricochet_colourtrans_audit_focused_config RICOCHET_DEMO_VOLUME=/tmp/ricochet_colourtrans_audit_focused_volume cargo test --no-default-features --test ricochet_colourtrans_ownership -- --test-threads=1` — passed 1/1.
- `RICOCHET_CONFIG_PATH=/tmp/ricochet_colourtrans_audit_graphics_config RICOCHET_DEMO_VOLUME=/tmp/ricochet_colourtrans_audit_graphics_volume cargo test --no-default-features --lib graphics_swis` — passed 1/1.
- `RICOCHET_CONFIG_PATH=/tmp/ricochet_colourtrans_audit_wimp_latest_config2 RICOCHET_DEMO_VOLUME=/tmp/ricochet_colourtrans_audit_wimp_latest_volume2 cargo test --no-default-features --lib wimp_redraw_routes_to_independent_window_surfaces_and_restores_task_default` — passed 1/1, including SetGCOL in active window redraw contexts.
- `RICOCHET_CONFIG_PATH=/tmp/ricochet_colourtrans_audit_wimp_jit_config2 RICOCHET_DEMO_VOLUME=/tmp/ricochet_colourtrans_audit_wimp_jit_volume2 cargo test --features experimental-jit --lib wimp_redraw_routes_to_independent_window_surfaces_and_restores_task_default` — passed 1/1.
- `RICOCHET_CONFIG_PATH=/tmp/ricochet_colourtrans_audit_jit_config RICOCHET_DEMO_VOLUME=/tmp/ricochet_colourtrans_audit_jit_volume cargo test --features experimental-jit --test ricochet_colourtrans_ownership -- --test-threads=1` — passed 1/1.
- `RICOCHET_CONFIG_PATH=/tmp/ricochet_colourtrans_audit_jitmandel_config RICOCHET_DEMO_VOLUME=/tmp/ricochet_colourtrans_audit_jitmandel_volume cargo test --features experimental-jit --lib mandelbrot` — passed 1/1; exercises the existing sample-specific JIT route that calls these names.
- `RICOCHET_CONFIG_PATH=/tmp/ricochet_colourtrans_audit_final_full_config RICOCHET_DEMO_VOLUME=/tmp/ricochet_colourtrans_audit_final_full_volume cargo test --no-default-features` — passed 219 library tests, 4 ignored GPU tests, and every integration/doc target. An earlier full attempt exposed the Help module-list snapshot missing the newly published `ColourTrans`; after the snapshot was reconciled, the complete rerun passed.
- `rustfmt --edition 2024 --check src/swi.rs tests/ricochet_colourtrans_ownership.rs` and `git diff --check` — passed.

The tests establish hosted register behavior, exact name-only ownership,
no numeric-ID publication, inactive/retired fail-closed behavior, and the
solo-shell and active Wimp-redraw graphics-state paths. They do not establish
native ColourTrans compatibility, exact indexed palette/tint behavior, or
live GUI/GPU output. Native numeric ColourTrans SWIs,
palette mutation, general ColourTrans services, and broader WP5.5 graphics
work remain deferred.
