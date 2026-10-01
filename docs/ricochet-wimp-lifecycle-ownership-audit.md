# Wimp task-lifecycle ownership audit

## Result

The bounded lifecycle slice is implemented as BASIC64-owned public policy for
`Wimp_Initialise` (`&400C0`), `Wimp_CloseDown` (`&400DD`), and
`Wimp_StartTask` (`&400DE`). Their active definitions are in
[`Wimp.bas64`](../modules/Wimp.bas64); the public dispatcher acquires the
active module definition and retained source generation before invocation.
An inactive published owner errors instead of falling through to the previous
numeric Rust handler. The mechanism boundary remains in `WimpServer` and
checked task memory/launch queues. This is a three-call slice, not a Wimp
migration or WP5.6 completion.

## Contract checked

The [PRM Wimp chapter](https://www.riscos.com/support/developers/prm/wimp.html)
defines Initialise inputs as R0 requested version, R1 `TASK` magic, R2 task
description, and (from version 300) R3 message list; it returns current version
in R0 and task handle in R1. CloseDown takes that task handle in R0 and `TASK`
in R1. StartTask takes an R0 pointer to a `*Command` string and returns a live
child handle or zero. The hosted module declaration now marks Initialise R1
INOUT so the returned handle survives contract application; independent JIT
testing caught and verified this correction.

`Wimp.bas64` owns supported Initialise version/magic validation, StartTask
verb/argument grammar, allowlist and result placement. Rust mechanisms validate
bounded caller buffers, register/close only the calling task, and enqueue typed
launch requests. Initialise accepts only versions 200, 300, and 310. It reads a
bounded description and, for version >=300, preflights a non-null, at-most-128
word zero-terminated list before registration. The native v300 contract
requires a list; the hosted profile accepts null for v300 and v310. The list is
validated but currently does not filter Wimp poll messages. Version 200 ignores
R3, matching the documented version boundary.

CloseDown requires R1=`TASK` and the caller's exact live R0 handle; hosted R0
returns zero. It clears the caller's menu/click/event/window/icon state. A
Wimp-launched guest keeps its host console and remains a guest task, but its
Wimp registration becomes inactive; an ordinary caller's Wimp state is
removed. Repeated close and cross-task handles fail without closing another
task. `task_exited` remains the final teardown path. The dispatcher also drops
closed-window raster surfaces while preserving default graphics owned by a
still-live task. The internal Wimp redraw/window-independence unit test also
checks CloseDown surface-map cleanup and restoration of the task-default
raster; this does not establish live host-window teardown behavior.

StartTask preflights the NUL/CR-terminated command (maximum 255 content bytes),
accepts ASCII space/tab separators, and supports only `*Commands`, `*BASIC`,
and `*BASIC <guest-path>`. A guest path may be quoted and is resolved through
the existing guest filesystem; the command-wide limit can impose a shorter
effective path limit than the mechanism's 255-byte path bound. Unsupported
commands, malformed quoting, controls, invalid UTF-8, bad pointers and
uninitialized/non-Wimp callers fail before queuing. The launch queue returns a
task handle and creates the task with ordinary rights; it does not inherit the
caller's privileged configuration capability. This intentionally narrows the
native arbitrary `*Command` behavior: shell commands and other application
launches are not implemented by this service. BASIC file launch uses the
existing loader and execution preferences.

### Quoted-path correction

The initial lifecycle audit missed a defect in the optional quoted-path branch:
after consuming the closing quote, the parser tested the last byte rather than
the saved quote state, then re-entered unquoted parsing and replaced the path
slice. The correction makes quoted and unquoted parsing mutually exclusive
using saved quote state and charges scan budget only when consuming bytes, so
lookahead does not count a separator or opening quote twice. Regression
coverage inspects the queued guest path (not only the returned handle) for both
quote styles around paths containing spaces, checks trailing spaces/tabs, CR
termination, UTF-8, exact 255-byte acceptance and 256-byte rejection, and
proves malformed paths queue no task and do not poison the next valid request.
The first independent post-fix run caught an off-by-one in the new 255-byte
case; the strict boundary assertion was retained and the counter corrected
before the passing validation below.

Capsule publication works without a Wimp backend; invoking a lifecycle service
then reports an unavailable-host-service error through the module/X-error
path. Existing non-migrated Wimp SWIs retain their prior hosted dispatch.
Desktop/Filer startup and browsing remain on the existing Wimp paths; the
desktop integration suite exercises the migrated Initialise entry through a
live `Runtime`, rather than relying on the test-only direct Rust helper.

## Validation evidence

Independent checks on the shared final source snapshot:

- `cargo test --no-default-features --test ricochet_wimp_lifecycle_ownership -- --test-threads=1` — 1 passed.
- `cargo test --features experimental-jit --test ricochet_wimp_lifecycle_ownership --test wimp_desktop -- --test-threads=1` — lifecycle 1 passed; Wimp desktop 8 passed.
- `cargo test --features experimental-jit --lib wimp_redraw_routes_to_independent_window_surfaces_and_restores_task_default` — 1 passed, including the CloseDown surface-map cleanup check.
- `cargo test --no-default-features -- --test-threads=1` — 219 passed, 4 ignored; every integration and doc-test target passed, including all eight `wimp_desktop` tests and the expanded quote/boundary regression. The first independent full run had caught five stale export-count expectations (33/27 versus the updated 36/30); those expectations were corrected before this passing rerun.
- `git diff --check` and scoped `rustfmt --edition 2024 --check src/swi.rs src/wimp.rs tests/ricochet_wimp_lifecycle_ownership.rs tests/wimp_desktop.rs` — clean.

The integration test verifies publication/source identity, headless bootstrap
and fail-closed invocation, X error behavior, Task-scoped initialization,
invalid-input atomicity, version-200 and hosted-null-list cases, allowed and
rejected launches including quoted paths, control-byte rejection,
ownership-checked CloseDown,
ordinary and Wimp-launched cleanup, and recovery after rejected calls.
`wimp_desktop` covers existing multi-task window/input behavior. No live GUI
acceptance is claimed. Remaining work includes Wimp Poll/window/icon/menu
ownership, filtering the message list, arbitrary StartTask commands, and
end-to-end interactive desktop validation; WP5.6 remains partial.
