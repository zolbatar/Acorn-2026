# Graphics SWI ownership audit

This audit covers only the existing hosted `OS_Plot` (`&45`) and
`OS_ReadPoint` (`&32`) services. It does not claim complete graphics
compatibility, completion of WP5.5, or completion of Phase 5.

## Ownership and dispatch

`modules/Graphics.bas64` owns both public SWI definitions. `PlotService`
masks R0 to the low plot-code byte and passes R1/R2 as signed 32-bit
coordinate bit patterns. `ReadPointService` owns Modern-profile rejection and
the public on-screen/off-screen result registers. The definitions import only
the protected `Host.Graphics.Plot` and `Host.Graphics.ReadPoint` mechanisms
under `GraphicsRaster`; the module also declares `RuntimeErrors` for structured
failures. The previous numeric Rust semantic arms are removed. A quiesced owner
fails with the module-inactive error; after retirement, the exports are absent
and the calls report `UnknownSwi`, rather than reaching a hidden Rust fallback.

Rust retains the bounded raster and presentation mechanism. Plotting and point
reads use a shared default raster in the non-Wimp/non-desktop shell, preserving
the existing interactive shell surface. In Wimp/desktop mode, the configured
display task retains that shared shell surface, other no-window tasks receive
bounded task-default rasters, and an active redraw uses the original task's
window raster. VDU input, Plot, and ReadPoint use the same selection policy.
Plot publication keeps the caller task and window identity. Per-task contexts
are capped at 64 and share the hosted aggregate pixel budget. The existing
internal Wimp redraw test invokes both migrated SWIs while the redraw window
context and clip are active, checks the plotted point through ReadPoint,
verifies independent windows, and checks default-context restoration. The
external integration fixture cannot construct the private Wimp desktop
task/context; this path is covered by the internal dispatcher test instead.

The BASIC `PLOT` statement and its related line/move/draw paths enter the same
`OS_Plot` module definition. Numeric SWI dispatch and named `SYS "OS_ReadPoint"`
also use the active module registry. There is no hosted BASIC `POINT` function.
VDU25 byte-stream plotting is a separate route: `OS_WriteC` enters the Console
module and the Rust `Host.Graphics.AcceptByte` VDU parser. This audit does not
claim that the VDU parser or every BASIC graphics path migrated to Graphics.

## Compatibility boundary

The primary references are the [RISC OS PRM VDU driver chapter](https://www.riscos.com/support/developers/prm/vdu.html)
and its [VDU code table](https://www.riscos.com/support/developers/prm/vducodes.html).
The PRM defines OS_Plot inputs as R0 plot command and R1/R2 coordinates; R0-R2
may be corrupted on return. The hosted implementation preserves all three
registers deterministically, an intentional stronger guarantee. Coordinates
use the existing logical graphics-unit space relative to the active graphics
origin. The low byte selects the plot action and the implementation accepts
the existing `GraphicsState` subset only. Unsupported plot groups/actions
remain explicit errors; the ownership change does not make them supported.

The PRM defines OS_ReadPoint coordinates in R0/R1, preserving both; R2 returns
colour, R3 tint, and R4 indicates whether the point is on screen. The hosted
module preserves R0/R1, returns the existing CPU-raster colour/tint and R4=0
for an in-surface point, and returns R2=R4=`&FFFFFFFF`, R3=0 outside the
surface. Modern text is an explicit error because the modern text overlay is
not stored in the guest raster. Indexed colour reads estimate the nearest
logical palette entry from RGBA; duplicate colours, exact tint and unsupported
plot actions are not exact. C16M reads return the hosted packed RGB value.

The raster is CPU-authoritative and reads are synchronous, before presentation.
Outside batching, plot calls publish the existing single Plot display event.
During the existing BASIC display batch, snapshot publication is rate-limited;
it is tied to the caller task/window and the presenter does not apply the plot a
second time. This preserves the existing render path; it is not a live visual
or GPU verification.

## Evidence and remaining limits

Independent checks on the current shared tree:

- `RICOCHET_CONFIG_PATH=/tmp/ricochet_graphics_swi_audit_config RICOCHET_DEMO_VOLUME=/tmp/ricochet_graphics_swi_audit_volume cargo test --no-default-features --test ricochet_graphics_swi_ownership -- --test-threads=1` — passed 1/1.
- `RICOCHET_CONFIG_PATH=/tmp/ricochet_graphics_swi_audit_final_jit2_config RICOCHET_DEMO_VOLUME=/tmp/ricochet_graphics_swi_audit_final_jit2_volume cargo test --features experimental-jit --test ricochet_graphics_swi_ownership -- --test-threads=1` — passed 1/1.
- `RICOCHET_CONFIG_PATH=/tmp/ricochet_graphics_swi_audit_desktop_lib_config RICOCHET_DEMO_VOLUME=/tmp/ricochet_graphics_swi_audit_desktop_lib_volume cargo test --no-default-features --lib swi::tests::graphics_swis_isolate_non_display_task_defaults_in_desktop_mode -- --exact` — passed 1/1; exercises the public module route under a Wimp-backed dispatcher and verifies detached task-default rasters.
- `RICOCHET_CONFIG_PATH=/tmp/ricochet_graphics_swi_audit_desktop_jit_config RICOCHET_DEMO_VOLUME=/tmp/ricochet_graphics_swi_audit_desktop_jit_volume cargo test --features experimental-jit --lib swi::tests::graphics_swis_isolate_non_display_task_defaults_in_desktop_mode -- --exact` — passed 1/1.
- `RICOCHET_CONFIG_PATH=/tmp/ricochet_graphics_swi_audit_wimp3_jit_config RICOCHET_DEMO_VOLUME=/tmp/ricochet_graphics_swi_audit_wimp3_jit_volume cargo test --features experimental-jit --lib swi::tests::wimp_redraw_routes_to_independent_window_surfaces_and_restores_task_default -- --exact` — passed 1/1; invokes OS_Plot/OS_ReadPoint inside each active redraw window and verifies clipping/context restoration.
- `RICOCHET_CONFIG_PATH=/tmp/ricochet_graphics_swi_audit_verified_full_config RICOCHET_DEMO_VOLUME=/tmp/ricochet_graphics_swi_audit_verified_full_volume cargo test --no-default-features` — passed 219 library tests, 4 ignored GPU tests, and all integration/doc tests. This run includes the corrected Help snapshot listing Graphics. An intermediate per-task-default revision regressed shell-context tests; the final context selector preserves the shared solo shell and isolates Wimp/desktop non-display callers, and the final suite is green.

Live window visuals, GPU output, HiDPI scaling, and interactive desktop
acceptance remain unverified. The VDU parser, raster algorithms, indexed backing/exact palette
and tint semantics, remaining plot actions, other graphics/ColourTrans/font
SWIs, and renderer remain Rust or deferred. BASIC POINT is unsupported. WP5.5
and Phase 5 remain partial.
