# Trellis caller-authorization audit

Audit status: the bounded WP5.3 module/source authorization gate is implemented
and its focused public integration test passes. I found no remaining caller-
authorization bypass in this slice. This is not a complete permission graph,
per-program sandbox, or host security boundary against arbitrary native Rust
code.

## Policy verified

| Operation | Ordinary `Task::new` | Source inspector | Module manager | Trusted MOS session |
|---|---:|---:|---:|---:|
| Active module title/version/state and exported SWI identity/owner/definition/generation | Public | Public | Public | Public |
| Current retained definition source, including private definitions | Denied | Allow | Denied | Allow |
| `OS_Module` Load/Delete; `*RMLoad`/`*RMRun`/`*RMKill`; `*RMEnsure` when its fallback tail performs a mutation | Denied | Denied | Allow | Allow |
| Mint/change a requestor grant from BASIC64, a guest manifest, or a numeric task ID | Denied | Denied | Denied | Denied |

Public metadata is an explicit policy decision: it describes currently
published service identities, not retained code or task state. Source and
mutation rights are independent; neither operation is blanket-public. Guest
modules cannot import protected ModuleManager host primitives by declaring a
capability, and such a provider capability never upgrades the original caller.

## Boundary and call-path findings

- `src/memory.rs` stores private authority bits on each `Task` object. The
  ordinary `Task::new(id)` constructor grants neither right;
  `trusted_source_inspector`, `trusted_module_manager`, and
  `trusted_mos_session` are explicit Rust host bootstrap choices. Reusing an
  authorized task's public numeric ID in a new ordinary Task grants nothing.
  No guest-visible grant/revoke operation or ID-indexed authority table exists.
- Production `Runtime::stdio/windowed` bootstraps the interactive shell as a
  trusted MOS session. `Runtime::desktop_task` and Wimp-launched desktop tasks
  construct ordinary Tasks; there is no implicit parent-right inheritance.
  The host revokes a grant by ending/replacing its Task; rights otherwise last
  for that object’s lifetime. In-place revocation and user/account ACLs are not
  part of this cut.
- `modules/ModuleManager.bas64` asks for `SourceRead` before
  `ReadDefinitionSource`, and for `ModuleManagement` before Load/Delete. The
  Rust primitive handlers in `src/swi.rs` independently recheck the same
  caller Task before parsing a source selector, reading a module path, or
  invoking load/unload. Thus BASIC64 is the public policy layer but not the
  sole enforcement layer. The guest CLI's read route calls the source query;
  its mutation routes call public `OS_Module`. Direct SWIs and star commands
  therefore converge on the same caller checks.
- Nested calls preserve `&mut Task` through imported BASIC64 definitions and
  SWI dispatch. `with_module_execution` temporarily changes only the active
  provider-module identity and restores it; it does not change the Task or
  rights. A guest forwarder with no host capability cannot launder the
  authority of ModuleManager. Dynamic module lifecycle hooks likewise receive
  the invoking Task; authority follows the requestor, not the code's provider.
- Denied source queries leave caller output buffers untouched and return the
  structured task-authorization error. Invalid source selectors and OS_Module
  path pointers are not read before their relevant authorization check (the
  source SWI still performs its declared logical-memory range validation at
  dispatch). Normal failures propagate as errors; X-form failures set V and
  populate the caller's error block/code/message. Public queries remain
  read-only. Boot lifecycle startup uses an ordinary temporary Task, while the
  restricted native recovery interface runs before public SWIs are exposed.

## Verification performed

Independent focused runs on the current shared snapshot, all with isolated
`ACORN_CONFIG_PATH` values:

```text
ACORN_CONFIG_PATH=/private/tmp/acorn-auth-audit-current-20260930-01.configure \
  cargo test --no-default-features --test trellis_authorization \
  trellis_services_enforce_task_scoped_read_and_management_authority -- --exact --nocapture
1 passed, 0 failed

ACORN_CONFIG_PATH=/private/tmp/acorn-auth-audit-current-20260930-02.configure \
  cargo test --no-default-features --lib \
  runtime::tests::interactive_runtime_bootstrap_is_trusted_but_spawned_desktop_task_is_not -- --exact
1 passed, 0 failed

ACORN_CONFIG_PATH=/private/tmp/acorn-auth-audit-current-20260930-03.configure \
  cargo test --no-default-features --lib \
  swi::tests::failed_foundation_start_rolls_back_the_complete_public_namespace -- --exact
1 passed, 0 failed

ACORN_CONFIG_PATH=/private/tmp/acorn-auth-audit-current-20260930-04.configure \
  cargo test --no-default-features --test trellis_mos_introspection \
  wp51_mos_introspection_matches_read_only_queries_and_tracks_live_generations -- --exact
1 passed, 0 failed

ACORN_CONFIG_PATH=/private/tmp/acorn-auth-audit-current-20260930-05.configure \
  cargo test --no-default-features --test trellis_wp51_replacement \
  wp51_os_module_replacement_is_atomic_compatible_and_preserves_active_calls -- --exact
1 passed, 0 failed

cargo fmt --all -- --check
passed
```

The authorization integration exercises ordinary, trusted MOS, source-only,
and management-only profiles; public metadata; source and management denials
through direct SWIs and `OS_CLI`; same-ID spoofing; nested forwarding calls;
guest capability-import denial; caller-memory isolation; no source/output or
registry/workspace/generation mutation on denial; normal and X-form error
contracts; authorized replacement/deletion; and task-profile bootstrapping.
The focused MOS and replacement regressions confirm the new checks did not
break the existing public query/management routes. These are targeted tests,
not a full-suite result.

## Scope limits and remaining work

This is task-principal authorization, not per-program isolation. BASIC code
run through `Runtime::run_application`/`run_guest_file` in a trusted interactive
session, and a guest module's `Start`/`Quiesce`/`Finalise` invoked by that
session, use the same Task principal and therefore its rights. That behavior
is deliberate and documented. Untrusted programs requiring demotion must run
as a separate ordinary Task; the current public runtime does not turn each
program into a separate principal automatically.

The `trusted_*` constructors are Rust host APIs and therefore rely on the
embedding host to choose them only at trusted bootstrap sites. This design
does not defend against arbitrary native Rust code in the process. Rights have
Task lifetime rather than revocable scopes/tokens; task teardown/replacement
is the available revocation mechanism. Further UI/browser query families and
entity-specific visibility policy remain future WP6 work. No browser exists in
this checkout, so browser-to-MOS policy parity is not tested.

No concrete defect remains in the bounded metadata/source/OS_Module boundary
reviewed here. No full test suite, hostile native embedding, per-program
demotion, or in-place grant-revocation test was performed; those are not
claimed as verified.
