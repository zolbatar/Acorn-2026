# Ricochet `*INSPECT` and classic module-command audit

Status: independent source review and focused public-command test completed for
the 2026-09-30 snapshot. This is a bounded hosted command slice, not a claim of
full RISC OS module compatibility.

## Contract basis

The authoritative [RISC OS PRM, Volume 1 Chapter 14: Modules](https://www.riscos.com/support/developers/prm/modules.html)
maps `OS_Module` reason 0 to `*RMRun`, 1 to `*RMLoad`, 3 to `*RMReInit`, 4 to
`*RMKill`, 8 to `*RMTidy`, 9 to `*RMClear`, 10 to insertion, 11 to `*RMFaster`,
and 12 to both `*Modules` and `*ROMModules`. It specifies `*RMLoad
filename [module_init_string]`; `*RMRun` loads then enters a runnable image,
falling back to load-only if it has no application entry; `*RMKill
title[%instantiation]`; and `*RMEnsure title version [command]`, which does
nothing for an installed equal/newer version, otherwise runs the optional
command or errors when none is supplied. The PRM also documents module
instantiations, ROM modules, and RMA addresses as real parts of that native
model.

The [PRM CLI chapter](https://www.riscos.com/support/developers/prm/cli.html)
defines final-dot abbreviations as any leading prefix, and warns that aliases
and module command order can change which command wins. Its `OS_CLI` contract
accepts NUL, LF, or Return terminators, preserves R0, and is non-reentrant; the
PRM command preprocessor strips leading spaces and any leading `*` characters.

## Verified implementation

| Surface | Actual behavior verified in source/tests | Compatibility boundary |
|---|---|---|
| `OS_CLI` owner and routing | `RicochetCommands.bas64` owns `&05`; it parses the bounded line and owns `*INSPECT`, the migrated classic commands, `*CONFIGURE`, and `*STATUS`. Unmigrated commands go through the narrow legacy adapter. It accepts NUL/LF/CR, preserves R0, enforces the 256-byte input bound, strips repeated leading stars/spaces, and uses task-local checked scratch which is released after normal and error dispatch. | This is not a complete CLI implementation: aliases, environment substitution, filing-system dispatch, and module-registered command tables remain outside the migrated policy. `*T.` continues to reach legacy `TYPE`; the obsolete `*RICOCHET` namespace is not a public alias. |
| `*INSPECT` | `MODULES`, `MODULE`, `SWI`, and canonical `DEFINITION` (with read-only `SOURCE` alias) call the shared ModuleManager read APIs. Listing/detail rows use active logical module/export identity and generation, not guessed addresses. `DEFINITION` selects retained current source; display controls are escaped. Attempts such as `*INSPECT LOAD/RELOAD/DELETE` report read-only/unsupported status and leave module identity and behavior unchanged. | `*INSPECT` is a Ricochet addition, not a historical command. Module/SWI metadata is public; retained source requires the caller's `SourceRead` right. Only current active definitions are inspectable. No browser exists yet, so future UI parity is not verified. |
| `*Modules` and abbreviations | The list is a logical manifest view of active title/version/state and does not fabricate RMA/workspace addresses. The focused test verifies final-dot aliases including `*Mod.`, `*M.`, `*I.`, `*INSPE.`, subverb abbreviation/ambiguity, exact-first forms, and repeated-star `**Modules`. | PRM abbreviations resolve against live aliases/module ordering; Ricochet has a fixed explicit table and reports collisions as ambiguous (for example `*RMR.` and `*INSPECT MOD.`), rather than depending on load order. This is documented and safer, but not byte-for-byte CLI resolution parity. |
| `*RMLoad` | Uses `OS_Module` reason 1 through the caller-checked shared manager. A guest path is resolved by the hosted HostFS source path to BASIC64 `&064` source. Same-title compatible reload uses Ricochet' atomic replacement contract: one published module remains, its source/export generation changes, and an incompatible candidate leaves old source, generation, behavior, and workspace intact. | Native PRM load consumes `&FFA` module images, accepts an init string, and kills the prior same-title module before initializing the new one. Ricochet accepts no init string and deliberately uses rollback-safe compatible replacement instead. This is a documented hosted semantic change, not native `OS_Module` emulation. |
| `*RMRun` | Routes to the hosted reason-1 load operation and states that it is load-only. The test confirms the module becomes active and its exports callable. | PRM runs the image's application-entry point if present. System Profile modules have no separate application-entry field; lifecycle `Start` is not an application entry. Therefore RMRun currently equals RMLoad and must not be described as executing an application. |
| `*RMKill` | Uses `OS_Module` reason 4; management authority is checked on the original caller. A full title removes the managed module and its SWIs. | `%instantiation` is rejected because Ricochet has no multi-instantiation or ROM-active/inactive model. The native command can select an instance and can deactivate a ROM module. |
| `*RMEnsure` | Parses bounded decimal `major.minor[.patch]` components and compares the requested tuple with the active manifest version. Equal/newer is a no-op; absent/older invokes the full tail through the internal BASIC64 dispatcher (not recursive `OS_CLI`); absent/older without a tail returns a CLI error. A nested guest call retains its original Task rights, so the tail does not launder `SourceRead` or `ModuleManagement`. | The PRM documents numeric versions and examples such as `2.01` and `0.51`, but not this full textual grammar. Ricochet maps these to manifest semantic-version tuples; it is a documented host profile, not a claim of identical historical version-number encoding. |
| Other classic names | `RMReInit`, `RMInsert`, `RMTidy`, `RMClear`, `RMFaster`, `ROMModules`, and `Unplug` have explicit unsupported diagnostics; tests verify no module generation/state change. RMLoad/RMRun initialization tails and RMKill `%instance` are likewise rejected without mutation. | These operations depend on absent lifecycle-restart, RMA, ROM, unplug, native-image, or instance semantics. They are not aliases for load/delete. |

The checked public integration test also covers direct and CLI identity
agreement, ordinary versus authorized callers, `OS_CLI` R0 preservation,
sentinel memory and dynamic-scratch cleanup, nested `RMEnsure` authority,
replacement rollback, old `*RICOCHET` rejection, and no mutation on unsupported
routes. A separate WP5.1 integration test covers active calls retaining their
old replacement generation while new calls observe the new generation.

## Audit result and remaining scope

I found no remaining in-scope routing, authorization, scratch-lifetime, or
replacement defect in the final snapshot. One interim gap—stripping only one
leading `*`—was reported to the implementation owner, changed to repeat the
PRM preprocessing, and regression-tested with `**Modules`.

The independently rerun focused command passed:

```sh
RICOCHET_CONFIG_PATH=/private/tmp/ricochet-classic-audit-stars-<unique>.configure \
  cargo test --no-default-features --test ricochet_classic_module_commands -- --nocapture
```

Result: 1 passed, 0 failed. The contract matrix above is intentionally narrower
than the full PRM family. Native `&FFA` images, initialization strings,
application entry, ROM and RMA listing/management, instances, and exact
load/reinitialise/tidy lifecycle semantics remain unsupported. The static
abbreviation table cannot model commands or aliases added dynamically by
future modules. A focused test of the nested `RMEnsure` depth limit (eight
levels and rejection of the ninth) remains useful; the current integration
tests cover conditional tails and preserved caller rights, not that exact
boundary. This audit does not establish browser/UI parity or full historical
OS_CLI, register, and error behavior.
