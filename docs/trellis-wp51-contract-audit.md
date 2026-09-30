# WP5.1 contract audit: modules, errors, and SWI identity

Date: 2026-09-29

This audit separates PRM requirements, observable hosted behavior, intentional
hosted substitutions, unresolved defects, and checks that are still pending.
It covers the bounded post-boot `OS_Module` and X-form error slice plus the
three System query services planned for WP5.1. It is not a claim of full RISC OS
compatibility. The module row below was first recorded before the 2026-09-30
replacement batch; the current replacement contract and test evidence are in
[`trellis-wp51-replacement-audit.md`](trellis-wp51-replacement-audit.md).

## Findings

| Contract | RISC OS PRM requirement | Trellis behavior in scope | Assessment |
|---|---|---|---|
| `OS_Module` reason 1, Load | `R0=1`; `R1` points to a pathname with optional parameters. The file must be a relocatable module of type `&FFA`. On success, `R0/R1` are preserved. | Accepts a checked caller-task pathname to UTF-8 BASIC64 source of type `&064`. New titles validate dependencies/capabilities, publish for `Start`, and become active only after success. Same-title active guest sources may use the bounded compatible-immediate replacement class: stable module/cell IDs, unchanged public and state schemas, shared workspace, atomic all-export publication, and retained old leases. | Reason and register positions are retained. Source format, file type, parameter policy, and transactional replacement semantics are hosted substitutions. PRM replacement is destructive; this class preserves the old active module on candidate rejection. See the linked replacement audit for detailed evidence. |
| `OS_Module` reason 4, Delete | `R0=4`; `R1` points to a module title, optionally including an instantiation. It deletes the preferred or named instantiation and preserves `R0/R1` on success. | Requires an exact full title, rejects `%` instantiations, protects foundation modules and active dependencies, then runs transactional `Quiesce` and `Finalise`. A failed `Quiesce` restores Active; a failed `Finalise` leaves a retryable quiesced module. | Reason/register shape and lifecycle purpose are retained. Exact-title matching, no instantiations, protected foundation modules, and retry semantics are deliberate hosted behavior. |
| Other `OS_Module` reasons | The PRM assigns reasons 0–20 to Run, Load, Enter, ReInit, Delete, RMA operations, enumeration, insertion, and ROM enumeration. Reasons 12 and 18 return module/private-workspace/postfix addresses. | Only reasons 1 and 4 are implemented. Other reasons return `UnsupportedServiceReason` before their reason-specific registers are read. Pointer-bearing reasons 12 and 18 remain unsupported; checked project queries provide semantic identity instead. | Deliberate, documented scope and a safety boundary. Project queries are versioned Trellis extensions, not replacements with binary-compatible outputs. |
| `OS_GenerateError` / `XOS_GenerateError` | `R0` points to a word-aligned error block no larger than 256 bytes: a 32-bit number and NUL-terminated message. Non-X generates an error and invokes the active handler without returning. For X form, the PRM says the only effect is setting V. | Reads through checked caller logical memory and raises a structured hosted error for non-X. A valid X call preserves the supplied `R0` block and sets V. Invalid blocks are converted to a checked task-local X error block. No `ErrorV` chain or active error handler exists. | Block layout and the valid X case are retained. Structured propagation and invalid-pointer error transport are deliberate hosted deviations. |
| Unknown numeric or named SWIs | An unrecognized SWI reaches UKSWIV. The default is `No such SWI`, but guest code may claim the vector. | Unknown calls produce generic error code 1 and a `no such SWI` message. X form returns a checked task-local error block in `R0` and sets V. There is no guest UKSWIV claim mechanism. | Default user-visible failure is similar. Generic code 1 and the missing claimable vector are hosted limitations. |
| Safe module/SWI identity | `OS_Module` reasons 12 and 18 expose module addresses/private data. Historical SWI conversion can also use module naming data. | `Acorn_ModuleInfo`, `Acorn_ModuleLookup`, and `Acorn_SwiInfo` return active manifest names, versions, lifecycle state, owner, definition, and generation through checked caller buffers or registers. They do not return host pointers. | Deliberate semantic extensions. They do not promise historical module numbers, instantiation identities, or address compatibility. |
| `OS_ReadMonotonicTime` (`&42`) | No inputs; `R0` returns centiseconds since power-on or hard reset, increasing until 32-bit wrap. | The new System definition returns the hosted monotonic clock in centiseconds in `R0`. The epoch is the hosted runtime/session, not a physical machine reset. | Register and unit shape are retained. Epoch substitution is deliberate; wrap and long-duration behavior still need separate coverage. |
| `OS_SWINumberToString` (`&38`) | `R0` is the SWI number, `R1` the output buffer, `R2` its capacity. Preserve `R0/R1`, return length in `R2`, and NUL-terminate. Bit 17 adds a leading `X`. PRM naming includes OS names and module SWIs; an unknown module number becomes `User`. | The new System definition converts only active manifest-owned SWI identities, writes through checked caller memory, and preserves the PRM register roles and X prefix. | Register, terminator, and X-bit behavior are retained for the bounded active namespace. General RISC OS name tables and the `User` fallback are intentionally not claimed. Exact capacity behavior is exercised by the integration test. |
| `OS_SWINumberFromString` (`&39`) | `R1` points to a name terminated by a control/space byte. Return the number in `R0`, preserve `R1`; leading `X` sets bit 17. Names are case-sensitive; unknown names error. | The new System definition resolves exact-case active manifest names and the optional leading `X` through a checked caller string. Unknown or inactive identities return a structured error. | Calling convention, X prefix, case sensitivity, and checked read are retained for active identities. The larger historical system/module name tables are not implemented. |

## Verification evidence

The independent regression is `tests/trellis_wp51_contracts.rs`. It uses the
public `SwiDispatcher::dispatch` entry point and task logical memory. Its
fixture volume and configuration file are unique under the operating-system
temporary directory.

Executed command (configuration path isolated for this run):

```sh
ACORN_CONFIG_PATH=/private/tmp/acorn-wp51-contracts-rerun-20260929.configure \
  cargo test --no-default-features --test trellis_wp51_contracts \
  wp51_public_contracts_cover_modules_errors_identity_and_system_queries -- --exact
```

Result: **passed**, 1 test, 0 failures. It verified checked invalid addresses
on `OS_Module`, `OS_GenerateError`, and identity queries; rejection of every
unsupported `OS_Module` reason, including pointer-bearing reasons 12/18;
register preservation on successful Load/Delete and query calls; X-form V and
error-block behavior; named and numeric unknown SWIs; failed `Start` rollback;
active manifest identity and its removal after Delete; exact and undersized
SWI-name output buffers; NUL termination and returned length; case-sensitive
input, control termination, and X prefix; and monotonic centisecond ordering.

The implementation lane also reported this focused source-unit test passing:

```sh
ACORN_CONFIG_PATH=/private/tmp/acorn-wp51-system-queries-redo-20260929-a4c3.configure \
  cargo test --no-default-features --lib \
  swi::tests::system_query_swis_use_active_manifest_identity_and_checked_buffers
```

The integration test does not prove full RISC OS behavior. Remaining checks
include exhaustive register/flag combinations, error-vector behavior, the
full historical SWI name tables and aliases, and monotonic counter wrap after
long uptime. Broader replacement classes/state migration, native images, and
`%` instantiations remain outside the bounded hosted slice.

## Deliberate hosted deviations

- `OS_Module` loads visible `&064` BASIC64 source rather than native `&FFA`
  relocatable images, and does not accept initialization parameters or `%`
  instantiations. Same-title loads support only the compatible-immediate
  transactional class documented in the replacement audit, rather than the
  PRM's destructive duplicate kill/reinitialization behavior.
- Only `OS_Module` Load and Delete are implemented. Historical RMA, insertion,
  module-enumeration, lookup, and ROM-enumeration reasons are rejected.
- Reasons 12/18 are not emulated because they return addresses and module
  private-word contents with no safe meaning in Trellis's task/module model.
  Project queries expose semantic identity without those pointers.
- Ordinary `OS_GenerateError` uses the hosted structured `RuntimeError` path;
  it does not invoke the RISC OS service-call/ErrorV/error-handler chain.
- Unknown SWIs have no claimable UKSWIV. The X transport uses code 1 for an
  unknown SWI, while structured service errors retain their own hosted codes.
- System query naming is limited to active manifest-owned exports. It does not
  emulate every historical OS name, module chunk alias, or unknown-module
  `User` result.
- The monotonic clock's epoch is hosted runtime/session start. It is not tied to
  host boot time or a hardware reset counter.

## Unresolved defects

The independent public integration test reproduced no source defect in the
audited slice. The remaining compatibility gaps listed above are unverified or
deliberately out of scope; a passing bounded regression is not evidence that
they conform. Any newly observed failure must be reported with its exact
reproduction, rather than reclassified as an intentional deviation.

## Authoritative references

- [RISC OS PRM: Modules](https://www.riscos.com/support/developers/prm/modules.html) — `OS_Module` reasons and module address/private-word results.
- [RISC OS PRM: Generating and handling errors](https://www.riscos.com/support/developers/prm/errors.html) — standard error-block layout, X form, error handler, and ErrorV.
- [RISC OS PRM: An introduction to SWIs](https://www.riscos.com/support/developers/prm/swis.html) — X bit and the unused SWI vector (UKSWIV).
- [RISC OS PRM: Conversions](https://www.riscos.com/support/developers/prm/conversions.html) — `OS_SWINumberToString` and `OS_SWINumberFromString`.
- [RISC OS PRM: Time and Date](https://www.riscos.com/support/developers/prm/timedate.html) — `OS_ReadMonotonicTime`.
