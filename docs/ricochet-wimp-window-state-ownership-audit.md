# Wimp window-state ownership audit

## Result

`Wimp.bas64` now owns the public policy and block encoding for `Wimp_OpenWindow`
(`&400C5`), `Wimp_CloseWindow` (`&400C6`), `Wimp_GetWindowState` (`&400CB`),
and `Wimp_SetExtent` (`&400D7`). Their old numeric Rust dispatch entries have
been removed from the public dispatcher. The module calls narrow
`WimpWindowState` mechanisms, which accept decoded scalar values and use the
original caller Task to check registration, window ownership, and state under
the Wimp lock. There is no Rust numeric fallback when the module is quiesced.
This is a four-call WP5.6 slice, not completion of Wimp or Phase 5.

## PRM contracts checked

The [official PRM Window Manager chapter](https://www.riscos.com/support/developers/prm/wimp.html)
specifies:

- OpenWindow takes R1 pointing to a 32-byte block: handle; visible area
  min/max coordinates; x/y scroll offsets; and the handle to open behind. The
  PRM accepts -1 (top), -2 (bottom), -3 (behind the backwindow), or a window
  handle. R0 is corrupted.
- CloseWindow takes R1 pointing to a four-byte block containing the handle,
  removes that window from the active list, and requests redraws for windows
  newly exposed. R0 is corrupted.
- GetWindowState takes R1 pointing to a 36-byte in/out block. It preserves the
  input handle and returns visible area, scroll offsets, the front window (or
  -1), and flags. R0 is corrupted.
- SetExtent takes the handle in R0 and R1 pointing to four signed coordinates
  in a 16-byte block. The new extent may not exclude any part of the currently
  visible work area; the Wimp updates the scroll-bar representation. R0 is
  corrupted.

The inventory's former CloseWindow description (“R1 window handle”) was
incorrect. The implementation and reconciled inventory now use the PRM's
pointer-to-four-byte-block contract.

## Ownership and mechanism review

The Wimp module declares caller-memory contracts for the full 32-byte Open
block, four-byte Close block, 36-byte GetWindowState read/write block, and
16-byte SetExtent block. BASIC64 decodes the words and signed coordinate
bit-patterns, applies the public stack policy, calls typed mechanisms, and
encodes all nine words of the state result. Rust does not receive the public
SWI number or a guest pointer for these operations. Runtime memory-contract
preflight checks each entire declared span before BASIC64 executes;
GetWindowState writes only after the mechanism has successfully built the full
result.

The typed Rust operations recheck that the caller is registered with Wimp and
owns the target window. OpenWindow validates geometry, visible-area/extent
consistency, hosted minimum dimensions and screen limits, and the stack target
before computing and committing a new stacking order. It then performs the
open-state transition, first-open invalidation and redraw scheduling. A
rejected request leaves the prior open-window snapshot unchanged. CloseWindow
checks ownership before closing, removing the window from the active stack,
updating focus and queuing redraws for exposed windows; it does not delete the
window definition. GetWindowState returns a complete standard-layout block,
including the dynamic open/visible/focus/toggle flags and control flags.
SetExtent validates the new geometry and ensures the current visible area stays
inside it before changing the extent or invalid regions; successful changes
schedule visible redraws.

The basic-profile/SYS routes use the same module-owned exports. X forms use the
common module error transport. Bootstrap still publishes the Wimp module in a
headless runtime; calling a window operation without a Wimp backend reports an
unavailable mechanism through that definition. The ownership fixture checks
module/source identity, quiesces Wimp, and confirms all four calls fail without
increasing transitional dispatch, rather than reaching the former Rust
handlers.

## Hosted deviations and correction

The hosted OpenWindow subset accepts -1, -2, or a positive live window handle
owned by the same Wimp task; it does not implement PRM's -3/backwindow
position. The existing hosted geometry limits and control-size minimums are
also stricter than an unrestricted native desktop. SetExtent gives successful
calls a deterministic R0=0 result instead of leaving its value unspecified.
These are explicit hosted choices; they are not claims of complete Wimp
compatibility.

The first independent run exposed a real ABI bug: the GetWindowState primitive
returned the flags word as unsigned U32, which clipped high-bit flags when
assigned to BASIC64's signed `%` local. The clipped value set open bit 16 even
after CloseWindow. The primitive result is now typed S32 so the raw 32-bit flag
pattern is preserved. The regression performs CloseWindow, queries the state
before reopening, and asserts that bit 16 is clear. It also verifies the
full nine-word initial block and a scroll position made valid only by the new
extent.

## Validation evidence

Independent runs against the final shared source snapshot:

- `cargo test --no-default-features --test ricochet_wimp_window_state_ownership -- --test-threads=1` — 1 passed.
- `RICOCHET_CONFIG_PATH=/private/tmp/ricochet-wimp-window-audit.NEpYCX/config.json RICOCHET_DEMO_VOLUME=/private/tmp/ricochet-wimp-window-audit.NEpYCX/volume cargo test --features experimental-jit --test ricochet_wimp_window_state_ownership --test wimp_desktop -- --test-threads=1` — ownership 1 passed; Wimp desktop 8 passed. The temporary volume was a private copy of `demo-volume`.
- `RICOCHET_CONFIG_PATH=/private/tmp/ricochet-wimp-window-audit.NEpYCX/config.json RICOCHET_DEMO_VOLUME=/private/tmp/ricochet-wimp-window-audit.NEpYCX/volume cargo test --features experimental-jit --lib wimp_redraw_routes_to_independent_window_surfaces_and_restores_task_default` — 1 passed.
- `RICOCHET_CONFIG_PATH=/private/tmp/ricochet-wimp-window-audit.NEpYCX/config.json RICOCHET_DEMO_VOLUME=/private/tmp/ricochet-wimp-window-audit.NEpYCX/volume cargo test --no-default-features --lib basic64_desktop_browses_scrolls_and_launches_mounted_programs` — 1 passed. This is an automated Filer workflow, not live GUI acceptance.
- `RICOCHET_CONFIG_PATH=/private/tmp/ricochet-wimp-window-audit.NEpYCX/config.json RICOCHET_DEMO_VOLUME=/private/tmp/ricochet-wimp-window-audit.NEpYCX/volume cargo test --no-default-features -- --test-threads=1` — 219 unit tests passed, 4 ignored; all integration and doc-test targets passed, including the new ownership fixture, eight Wimp desktop tests, and `runtime::tests::basic64_desktop_browses_scrolls_and_launches_mounted_programs`.
- `rustfmt --edition 2024 --check src/swi.rs src/wimp.rs tests/ricochet_wimp_window_state_ownership.rs tests/wimp_desktop.rs` and `git diff --check` — clean.

The fixture verifies real BASIC `SYS` block behavior, full state fields,
post-SetExtent scroll acceptance, close/reopen state, cross-task rejection for
all four operations, invalid geometry/extent and pointer failures, unchanged
state and sentinel output on query failure, X error behavior, module route and
quiesced no-fallback behavior. Existing Wimp desktop and Filer workflow tests
are automated evidence only. No live GUI acceptance is claimed.

Wimp creation, icons, Poll/input, redraw/update, menus, pointer queries, and
other Wimp policies remain outside this migration. Native -3 stacking and
complete RISC OS Wimp compatibility remain open; WP5.6 and Phase 5 remain
incomplete.
