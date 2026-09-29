# Trellis SWI compatibility test matrix

This matrix links the hosted dispatcher inventory in
[`trellis-swi-inventory.yaml`](trellis-swi-inventory.yaml) to executable
checks. “Partial” means the current hosted implementation or its tests cover a
useful slice, not every historical reason, flag, buffer limit, or error case.

| Service family | Automated evidence | Status and remaining gap |
|---|---|---|
| `OS_WriteC`, `OS_WriteS`, `OS_Write0`, `OS_NewLine` | `swi::tests::console_swis_dispatch_through_interpreted_module_definitions`; `swi::tests::console_definition_replacement_keeps_the_entry_cell_and_rejects_bad_contracts`; `swi::tests::console_string_swi_enforces_the_declared_terminator_bound`; Wimp redraw tests | Module owned for this subset. The source implements byte streaming, caller-scoped typed addresses, checked NUL scans, inline PC alignment, and the legacy 4096-byte missing-terminator error at the original string address. `Host.Graphics.AcceptByte` contains VDU stream parsing; `Host.Console.WriteByte` is raw host output. |
| `OS_ReadC` | `swi::tests::console_swis_dispatch_through_interpreted_module_definitions`; `tests/mos_calls.rs::mos_readc_consumes_input_without_overwriting_basic_registers`; `tests/mos_calls.rs::basic_prefetched_input_remains_available_to_mos_readc` | Module owned; byte return, Escape carry and the MOS input queue bridge are checked. |
| `OS_ReadLine` | `swi::tests::os_read_line_is_a_module_definition_with_checked_caller_memory`; `swi::tests::os_read_line_preserves_eof_and_range_echo_contracts`; `swi::tests::os_read_line_keeps_legacy_range_clamping_and_control_d_behavior`; `swi::tests::os_read_line_substitutes_r4_for_echo_without_changing_buffered_input`; `swi::tests::os_read_line_reports_caller_memory_boundary_failures` | Active `Console.ReadLine` BASIC64 definition. Tests cover module routing, backspace editing, accepted-byte range and legacy 8-bit clamping, echo-only and R4 echo-substitution flags, full-buffer bell, Escape carry, CR/LF termination, Control-D/EOF after partial input, and checked task memory. Exhaustive combinations of historical option bits and exact legacy error-block details remain unverified; the initial contract is the behavior exercised by these compatibility tests. |
| MOS `CALL` entrypoints | `tests/mos_calls.rs` (timer round trip, OSWORD pointer checks, tokenized bridge, character services, invalid entrypoints, shared clock, OSBYTE queue/masks and input preservation); `tests/basic_jit_strict.rs` checked clock call | The currently supported hosted call-address subset is covered. Legacy file-control-block adapters and other machine-code calls remain out of scope. |
| `OS_Byte` / `OS_Word` | `tests/mos_calls.rs::osbyte_keyboard_queue_and_cli_use_existing_services`; `osbyte_masks_inputs_preserves_registers_and_reports_input_status`; `call_osword_uses_checked_full_and_split_pointers` | Partial. Only the listed input, keyboard, and clock reason subsets are implemented. |
| `OS_CLI` and configuration | `swi::tests::configure_accepts_supported_values_and_conf_abbreviation`; desktop command/configuration tests; `tests/mos_calls.rs` CLI bridge | Covered for the hosted command set and saved configuration. Full MOS command vocabulary is not implemented. |
| FileSwitch (`OS_File`, `OS_Args`, `OS_BGet`, `OS_BPut`, `OS_GBPB`, `OS_Find`, `OS_FSControl`) | `swi::tests::acorn_desktop_catalogue_uses_checked_guest_buffers_and_hostfs_metadata`; guest-file load paths and `filesystem` tests | Partial. Checked caller buffers and HostFS paths are used. Historical actions, FileSwitch filing systems, exact error blocks, and exhaustive reason-specific result registers need dedicated cases. |
| Graphics (`OS_Plot`, `OS_ReadPoint`) | `swi::tests::os_read_point_reads_immediate_pixels_and_preserves_coordinates`; Wimp redraw surface and clip tests | Partial. Immediate reads, plotting integration and Wimp surface routing are checked; all plot actions, VDU interactions, clipping edges and colours are not exhaustive. |
| ColourTrans | graphics snapshots and Wimp visual operations | Incomplete. `COLOURTRANS_ConvertHSVToRGB` and `SetGCOL` lack direct API tests; `WritePalette` is currently a hosted no-op. |
| Wimp task/window/event calls | `src/wimp.rs` unit tests; `src/swi.rs` Wimp integration tests; `tests/wimp_desktop.rs` | Broad hosted coverage for the implemented two-task window/event slice. It is not a complete RISC OS Wimp implementation; see the Wimp module documentation and explicit unsupported-operation errors. |
| `ACORN_DESKTOP` / `ACORN_DISPLAY` | `swi::tests::acorn_desktop_catalogue_uses_checked_guest_buffers_and_hostfs_metadata`; `swi::tests::acorn_display_swi_queries_applies_and_reports_save_failure_in_registers`; desktop command/configuration tests | Covered for the hosted custom service contracts. These project services are named-only and are not historical RISC OS SWIs. |
| Error and X-bit behavior | `RuntimeError` propagation through BASIC/SWI tests and selected API error-register cases | Partial. The hosted dispatcher primarily returns Rust `Result`; a systematic historical X-bit/error-block matrix has not been implemented. |
| BASIC64 portable typed IR boundary | `basic_compat::system_profile::tests::console_typed_ir_preserves_identity_checked_addresses_and_backend_boundary`; `basic_compat::system_profile::tests::records_flags_typed_results_readonly_and_structured_errors_execute`; `swi::tests::linked_module_proc_and_fn_imports_are_executable_and_scoped`; `swi::tests::opaque_resource_handles_check_identity_owner_and_rights_when_used`; `basic_compat::jit::system_profile_boundary_tests`; `basic_compat::strict_jit::system_profile_boundary_tests` (with `experimental-jit`) | The IR carries source/dependency identity, type/signature/workspace metadata, complete statement/expression payload (no parser AST payload), checked task-owned addresses, exact integer literals, and linked call kinds. Module execution is reconstructed from the IR into the shared interpreter; imported calls and resource-use rights have focused runtime tests. JIT/Strict/AOT reject native System Profile lowering explicitly. The IR is not a serialized package ABI or native module target. |

## Minimum verification commands

The first module slice can be checked independently with:

```sh
cargo test --no-default-features --lib trellis::tests
cargo test --no-default-features --lib basic_compat::system_profile::tests
cargo test --no-default-features --lib swi::tests::console_swis_dispatch_through_interpreted_module_definitions
cargo test --no-default-features --lib swi::tests::console_definition_replacement_keeps_the_entry_cell_and_rejects_bad_contracts
cargo test --no-default-features --lib swi::tests::console_string_swi_enforces_the_declared_terminator_bound
cargo test --no-default-features --lib swi::tests::os_read_line_
```

The existing full project suite remains the broad compatibility gate. The
inventory intentionally records missing focused cases instead of treating
successful whole-project tests as proof of complete historical conformance.
