# Ricochet command compatibility corrections audit

Audit snapshot: 2026-09-30. This is a bounded correction review, not a claim
of full RISC OS command or UI compatibility.

## Contract checked

The [RISC OS PRM CLI chapter](https://www.riscos.com/support/developers/prm/cli.html)
defines a final-dot command abbreviation as a leading-name match, resolved by
the first matching command in module search order. The [RISC OS 3.7 User
Guide](https://www.riscos.com/support/users/userguide3/book3b/book3_2.html)
demonstrates the distinction: Help can list multiple prefix matches, while
execution chooses the first (its `Configure`/`Continue` example). These sources
support one ordered lookup source for Help and execution; Ricochet's specific
priority order is established by its compatibility tests, not mandated by the
PRM.

Ricochet's preserved priorities are:

| Prefix | First descriptor | Other matching descriptors exercised |
|---|---|---|
| `BA.` | `BASIC` | `BASIC64` |
| `D.` | `DIR` | `DELETE`, `DISC`, `DESKTOP` |
| `F.` | `FILETYPE` | `FX` |
| `R.` | `RUN` | `RMLOAD`, `RMRUN`, `RMKILL`, `RMENSURE`, `RMREINIT`, `RMINSERT`, `RMTIDY`, `RMCLEAR`, `RMFASTER`, `ROMMODULES`, `RENAME` |

## Verified implementation and behavior

- `ModuleRegistry::active_commands()` in `src/ricochet.rs` orders active module
  titles case-insensitively, then keeps each manifest's declaration order.
  `RicochetCommands.bas64` restores the established priorities by declaring
  `BASIC` before `BASIC64`, `DIR` before the other `D.` entries, `RUN` before
  `RM*` and `RENAME`, and `FILETYPE` before `FX`. BASIC64 execution and Help
  query the active command registry; no separate prefix-priority dispatcher was
  introduced. Execution picks the first matching row; Help reports all matches.
  Exact command lookup remains distinct from final-dot abbreviation lookup.
- The command invocation boundary now forwards the selected registry command
  identity and raw argument tail to two-string BASIC64 handlers. One-string
  handlers remain supported; parser/load validation accepts only one or two
  typed `STRING` parameters. `UnsupportedModuleEntrypoint` no longer relies on
  ambient `COMMAND_NAME$`. The built-in registry identifier is uppercase (for
  example, `ROMMODULES`), while its syntax/help text may use `*ROMModules`; the
  diagnostic reports the selected canonical identifier. Direct and nested
  `RMEnsure ... *ROMModules` cases preserve that identity rather than printing
  `Unsupported *:`.
- `SwiDispatcher::invoke_registered_command` passes the original `&mut Task`
  through nested dispatch, saves the prior command context, and restores it
  after either successful or error return. The registry Invoke primitive
  re-resolves the owner/name/handler against the current active registry before
  calling it. Direct and nested ordinary-task `RMLoad` attempts remain denied
  and do not publish the guest module.
- The real stdio test exposed an additional defect that direct dispatcher tests
  missed: BASIC64's long `OS_ReadLine` execution could poll opportunistically
  and consume a queued byte from the command line. The scoped key-poll guard now
  covers both module-owned `OS_CLI` and `OS_ReadLine`, restoring the previous
  setting on return. The full stdio sequence—including nested `RMEnsure`,
  subsequent commands, and `*QUIT`—now completes without the dropped-character
  failure.
- OS_CLI command scratch is released by the dispatch boundary on success and
  error. Regression assertions preserve sentinel bytes and check no dynamic
  command scratch remains after dispatch.

## Evidence

The new `tests/ricochet_command_compatibility_regressions.rs` exercises Help's
all-matches order and first-match execution for all four prefixes, exact
`BASIC`/`BASIC64`/`DESKTOP`, the unsupported module-command identifiers, nested
RMEnsure identity, ordinary direct/nested management denial, scratch guards,
and the public stdio path. The previously failing tests also pass:

- `swi::tests::basic64_command_does_not_capture_existing_basic_abbreviations`
- `swi::tests::desktop_abbreviation_does_not_steal_the_existing_dir_abbreviation`
- `ricochet_configuration_corrections::configure_routes_are_single_shot_and_wimp_mode_is_the_only_display_setting`

I independently ran the complete suite with isolated configuration and demo
volume inputs:

```text
RICOCHET_CONFIG_PATH=/private/tmp/ricochet-command-compat-audit-20260930-141859.configure RICOCHET_DEMO_VOLUME=/private/tmp/ricochet-command-compat-audit-volume-20260930-141859 cargo test --no-default-features
```

Result: exit 0; library tests **212 passed, 0 failed, 4 ignored**; every binary,
integration, and doc-test target passed, including the new command regression
and all three named existing failures. The unique demo-volume path was a
temporary symlink to the repository's normal `demo-volume`; no project data was
modified. The targeted `rustfmt --edition 2024 --check` over changed Rust files
and `git diff --check` also passed. I did not run repository-wide `cargo fmt`;
the known unrelated formatting differences remain outside this audit.

## Remaining limits

This correction restores Ricochet's tested priority choices, not native RISC OS
module load ordering: Ricochet currently uses deterministic case-folded title
order across active modules and declaration order within a module. Help still
streams without terminal paging/task-window scroll integration. Obey/Exec
scripts, their variable/error provenance, and native ROM/RMA module semantics
remain deferred; explicitly unsupported native commands still report that
limitation rather than pretending to implement it. No broader phase-completion
claim is made.
