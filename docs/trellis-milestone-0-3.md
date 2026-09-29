# Trellis first module milestone: Phases 0–3

## What this demonstrates

This runnable slice proves that a public service can have an inspectable
BASIC64 definition while the rest of the hosted system continues through an
explicit transitional dispatcher:

1. `SwiDispatcher` parses and validates [`../modules/Console.bas64`](../modules/Console.bas64),
   links its declared primitives and grants, publishes its six exports, then
   starts the module.
2. A numeric call such as `OS_WriteC` resolves the registry entry cell and runs
   the current interpreted `Console.WriteC` generation.
3. String and line-input policies, newline composition, input results,
   caller-scoped guest memory, and the `OS_ReadC` Escape carry result are
   handled by BASIC64 source.
4. A private primitive call crosses the gateway only while this active module
   is executing and only for an import resolved with the necessary capability.
5. A compatible definition replacement preserves the SWI entry-cell identity,
   advances its generation, and allows a retained old invocation to finish.
6. Quiescence stops new calls; retirement waits for active leases, then removes
   the retired module's SWI names and numbers from publication.
7. A failing `Quiesce` restores private workspace and reopens SWI admission. A
   failing `Finalise` restores workspace state but keeps the module quiesced,
   exports inaccessible and source retained for repair/retry; retirement is
   rejected before running `Finalise` if quiescence has not succeeded.
8. Console source lowers to a separate typed, source-located IR. Interpreter
   invocation adapts that IR into the common BASIC runtime; Hybrid/Strict JIT
   requests cross the same boundary and reject native System Profile lowering
   explicitly.
9. Focused System Profile tests also execute linked cross-module PROC/FN
   imports, enforce resource-handle rights at use, scope typed immutable locals
   across PROC/FN calls, and check full-width signed/unsigned arithmetic and
   integer loops without floating-point rounding.

The demonstration is intentionally not a boot demo: the Rust constructor still
installs the embedded Console source and Rust Console handlers remain as
fallback code. `OS_ReadLine` is module-owned, but all other public services not
listed above remain on the explicit transitional route. The phase-4 boot
capsule and empty-table startup are not implemented. Console's VDU stream
policy is isolated as `Host.Graphics.AcceptByte`, distinct from raw host output
through `Host.Console.WriteByte`.

The trusted host-side `Basic64ModuleManager` can quiesce/retire modules and
replace a Console definition while running. It accepts a compatible replacement,
rejects a changed public contract without changing the active generation, and
restores the original source without restarting. The private authority token is
not exposed to guest BASIC64. These are management APIs and tests, not yet a
user-facing live editor.

## Exercise the slice

```sh
cargo test --no-default-features --lib trellis::tests
cargo test --no-default-features --lib basic_compat::system_profile
cargo test --no-default-features --lib console_typed_ir_preserves_identity_checked_addresses_and_backend_boundary
cargo test --no-default-features --lib uint64_and_int64_literals_arithmetic_and_comparisons_remain_exact
cargo test --no-default-features --lib local_readonly_binding_is_available_in_function_bodies
cargo test --no-default-features --lib readonly_local_record_cannot_be_mutated_through_a_field_place
cargo test --no-default-features --lib classic_and_hybrid_integer_literals_keep_the_floating_number_model
cargo test --features experimental-jit --lib system_profile_boundary_tests
cargo test --no-default-features --lib swi::tests::console_
cargo test --no-default-features --lib swi::tests::os_read_line_
cargo test --no-default-features --lib swi::tests::linked_module_proc_and_fn_imports_are_executable_and_scoped
cargo test --no-default-features --lib swi::tests::opaque_resource_handles_check_identity_owner_and_rights_when_used
cargo test --no-default-features --lib swi::tests::guest_primitive_invocation_is_rejected_outside_an_active_module
cargo test --no-default-features --lib swi::tests::failing_quiesce_restores_active_state_workspace_and_swi_admission
cargo test --no-default-features --lib swi::tests::failing_finalise_keeps_quiesced_module_retryable_without_partial_cleanup
```

The first command covers identity, publication, capability checks, same-named
private definitions, lifecycle quiescence/retirement, and a real threaded
replacement. The Console tests check dispatch ownership, result registers, byte
display events, caller-scoped memory bounds, line editing/input edge cases,
lifecycle workspace, replacement/restore behavior, and rejection of an
incompatible contract. Set `ACORN_CONFIG_PATH` to a temporary test path when
running the no-feature commands if the saved user configuration selects an
experimental engine. Start failure is separately tested for atomic removal
of every published export and workspace rollback. Quiesce and Finalise failure
paths have independent rollback/retry tests. The IR test verifies module/source
identity, typed signatures, caller-owned checked address operations, and the
explicit interpreter-versus-compiled backend boundary.

The compatibility inventory and its remaining focused-test gaps are in
[`trellis-compatibility-matrix.md`](trellis-compatibility-matrix.md); package
completion boundaries are in [`trellis-work-packages.md`](trellis-work-packages.md).
