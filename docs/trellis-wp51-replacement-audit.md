# WP5.1 guest-module replacement audit

Date: 2026-09-30

Scope: post-boot `OS_Module` reason 1 reloading an active same-title guest
module from bounded HostFS BASIC64 `&064` source under the
`compatible-immediate` policy. Native `&FFA` modules, init parameters, `%`
instantiations, state migration, and user-authorized editing are outside this
slice; state migration remains Phase 7 work.

## PRM contract

The RISC OS PRM defines reason 1 as `R0=1`, `R1` pointing to a pathname plus
optional initialization parameters, and preserves `R0/R1` on success. The file
must be a relocatable module of type `&FFA`. If a module with the same title is
already loaded, RISC OS attempts to kill it first; all its instantiations are
removed, then the new module is initialized as `Base`. A refusal to finalize or
a failure to initialize can fail the load. The PRM therefore does not promise
rollback to the old module after a failed replacement. Module-title matching is
case-insensitive. A replacement module is expected to retain the same entry
points and return behavior while its internals may differ.

Trellis deliberately substitutes checked BASIC64 source for native `&FFA`
images and provides stronger staged rollback and retained-call behavior. The
authoritative references are the [RISC OS PRM, Volume 1, Chapter 14:
Modules](https://www.riscos.com/support/developers/prm/modules.html) and
[Chapter 1: An introduction to RISC OS](https://www.riscos.com/support/developers/prm/intro.html).

## Verified behavior

- The guest loader reads a checked task-local path, limits source to 4 MiB,
  requires valid UTF-8 and file type `&064`, and parses the declared module
  title. Lookup follows PRM case-insensitive module-title matching; a case-only
  candidate is normalized to the installed spelling. A same-title active
  module routes to the replacement path. Foundation modules, non-Active
  modules, unsupported policies, and capability-bearing replacements are
  rejected.
- The candidate must preserve the existing manifest except for source path and
  source hash. This protects module version, dependencies, imports/exports,
  SWI names/numbers/register and memory contracts, capabilities, lifecycle, and
  replacement policy. The candidate must still resolve its declared active
  module and symbol dependencies (including `FN:` imports). Exported PROC/FN
  and lifecycle signatures are checked, as are the persistent-state
  declarations and complete named type table.
- Registry replacement validates the complete candidate before swapping every
  exported SWI generation. The module ID and SWI entry-cell IDs remain stable;
  the module record, manifest, and current definition descriptors advance as
  one exclusive registry operation. New dispatches resolve through the new
  generations. A reentrant test reloads from inside an old-generation SWI call:
  that frame completes its old behavior, and later calls execute the new
  source. The old descriptor and source remain retained until its lease drains;
  derived targets are invalidated by old definitions and the new dependency
  fingerprint.
- `ModuleWorkspace` is shared across generations, not reinitialized. The
  public-path test mutates persistent state before reload, reads it from the
  replacement, and checks the candidate's deliberately failing `Start` hook
  was not run. Immediate replacement intentionally does not run candidate
  `Start` or old `Quiesce`/`Finalise`; doing so could mutate retained state or
  perform effects this transaction cannot undo. This differs from PRM reason
  1's kill/finalize then initialize sequence and is a hosted policy choice.
- All ordinary rejection checks precede generation publication. Registry
  publication prevalidates the entire export set under exclusive mutable
  access, then commits every cell and module descriptor without a normal
  fallible step between exports. The integration test rejects changed SWI
  contract, workspace declaration, capability, dependency, and lifecycle
  metadata and verifies prior identities, generation, source and behavior
  remain active. Foundation Console replacement is also rejected without
  changing its cell or emitted output.

## Residual limits

No replacement defect was reproduced in the reviewed snapshot. Automated
negative cases reject a changed named `RECORD` layout, an exported FN parameter
type change, and mutations to the public SWI contract, persistent-state
declaration, capability, dependency, and lifecycle metadata. The active-call
case uses nested dispatcher reentry; it does not test genuinely concurrent
guest dispatch across threads. State migration remains Phase 7; unequal
schemas are rejected rather than converted. The immediate path does not run
lifecycle hooks, so it does not attempt to roll back arbitrary external effects
from hooks.

## Verification evidence

The replacement-specific public dispatcher test is
`tests/trellis_wp51_replacement.rs`. It exercises a four-export compatible
reload and stable module, instance, entry-cell, owner, definition and SWI
identities; atomic generation advance; old-frame completion across nested
`OS_Module` reload; new-source behavior on subsequent calls; shared workspace;
case-insensitive title matching; an active imported FN dependency; rejection
rollback for changed SWI contract, scalar state, named type layout, exported FN
signature, capability, dependency and lifecycle metadata; and foundation
protection. Successful direct and case-only reloads preserve reason-1 R0/R1.
A separate unit test retains an explicit old invocation lease across
replacement and checks retired-source cleanup after it drains.

Commands run with isolated configuration paths:

```sh
ACORN_CONFIG_PATH=/private/tmp/acorn-trellis-replacement-audit-final-20260930.configure \
  cargo test --no-default-features --test trellis_wp51_replacement -- \
  --exact wp51_os_module_replacement_is_atomic_compatible_and_preserves_active_calls

ACORN_CONFIG_PATH=/private/tmp/acorn-trellis-replacement-audit-contracts-20260930.configure \
  cargo test --no-default-features --test trellis_wp51_contracts

ACORN_CONFIG_PATH=/private/tmp/acorn-trellis-replacement-audit-unit-20260930.configure \
  cargo test --no-default-features --lib module_manager_
```

Results: replacement integration **passed (1)**; existing WP5.1 public-contract
integration **passed (1)**; module-manager unit selection **passed (5)**. The
replacement integration was rerun after named-type and exported-signature
negative cases were added and passed. Only pre-existing dead-code warnings were
emitted. The work-package checkpoint continues to mark WP5.1 partial, not
complete.
