# Ricochet command registry and `*HELP` audit

Audit snapshot: 2026-09-30. This reviews the module-owned command registry,
`*HELP`, and the current command execution bridge. It does not claim complete
RISC OS CLI, script, or interactive UI compatibility.

## Contract basis

The RISC OS 3.7 User Guide describes `*Help` as the command/help index;
`Commands`, `FileCommands`, `Modules`, and `Syntax` are standard topics.
`*Help keyword` gives the command syntax and explanation. A dotted prefix
query lists every match, while execution chooses the first match in the
environment's command/module order. The guide also defines `<value>`, `[value]`,
and `|` notation. Its command-line pager pauses a terminal, whereas a Task
window is a separately scrollable text surface. See the [User Guide command
line chapter](https://www.riscos.com/support/users/userguide3/book3b/book3_2.html).

The PRM specifies case-insensitive command matching, final-dot prefix
abbreviation, and resolution through the kernel/module list (with module
order as shown by `*Modules`) before filing-system/service/Run fallback. It
also specifies leading-star/space handling and other preprocessing such as
aliases, redirection, `/Run`, `%`, and filing-system context overrides. Ricochet
now provides the bounded static-string alias subset in the
[CLI alias audit](ricochet-cli-aliases-audit.md), but not the other facilities
or full native module ordering. See
the [PRM CLI chapter](https://www.riscos.com/support/developers/prm/cli.html).
The [PRM Modules chapter](https://www.riscos.com/support/developers/prm/modules.html)
describes module titles and a shared Help/command keyword table; module title
matching is case-insensitive and command spelling is retained for readable
Help. Its historical module listing may include creation dates, which this
runtime does not have and must not invent.

The user-linked [RISC OS Open `*Help` page](https://www.riscosopen.org/wiki/documentation/show/*Help)
was inaccessible to the web reader during this audit. The RISC OS 3.7 User
Guide and official RISC OS PRM pages above were accessible and used instead;
no additional semantics are inferred from the inaccessible page.

## Implemented boundary and evidence

| Area | Verified implementation | Evidence and limit |
|---|---|---|
| One command/help inventory | `@COMMAND` metadata is parsed into each `ModuleManifest`. `ModuleRegistry::active_commands()` derives the published ordered list from active modules, sorted by case-folded module title and then declaration order. The BASIC64 `RicochetCommands` OS_CLI owner uses `Host.CommandRegistry.ReadEntry` for Help and selection, then `Invoke` revalidates the active owner/name/handler before dispatch. | `src/ricochet.rs` (`ModuleCommand`, `ModuleManifest`, `active_commands`, `active_command`); `src/swi.rs` (`HOST.COMMANDREGISTRY.READENTRY/INVOKE`); `modules/RicochetCommands.bas64` (`FindCommandDescriptor`, `CommandHelp`). This is one live source for public matching and Help, not a startup snapshot. |
| Module publication/replacement/removal | Command rows are derived from the owning active module, so they appear/disappear with that module. Compatible replacement keeps command name/category/syntax/handler identity and ordering while permitting description-only edits; incompatible replacements roll back with the module. A command-only module with a private BASIC64 handler is supported. | `same_replacement_manifest` and active registry code in `src/ricochet.rs`; public fixture in `tests/ricochet_command_help.rs`. The handler is not automatically a public symbol export. |
| Matching and order | Matching ignores ASCII case. A final-dot prefix is execution syntax; execution chooses the first active descriptor in hosted order. `*HELP prefix.` lists every matching descriptor. Exact command spelling does not use prefix matching. | `DispatchCliText`, `FindCommandDescriptor`, and Help scan in `modules/RicochetCommands.bas64`; colliding guest commands and Help/execution assertions in `tests/ricochet_command_help.rs`. Hosted order is alphabetical module-title order, then declaration order; this is deterministic and also the order displayed by the hosted `*Modules`, but is not a promise to reproduce native RISC OS load-order evolution. |
| Topics and module Help | Bare Help, `Commands`, `FileCommands`, `Modules`, and `Syntax` are registry-backed. Syntax Help explains required `<value>`, optional `[value]`, and alternatives `|`, then enumerates registered command syntax. Module Help resolves the active module case-insensitively and prints its canonical installed title/version plus its commands. No creation dates, RMA addresses, or other unavailable fields are fabricated. | `CommandHelp`, `FindHelpModule`, `MaybePrintHelpEntry` in `modules/RicochetCommands.bas64`; test snapshots and mixed-case module selection in `tests/ricochet_command_help.rs`. The ModuleLookup one-based ordinal is converted to the zero-based ModuleInfo index. |
| Rust transition bridges | Remaining migrated-later handlers are explicit `BRIDGE` descriptors in the trusted `RicochetCommands` manifest and a closed Rust command allowlist. The registry miss is BASIC64 `Bad command`; it does not enter a generic Rust handler or try to run an arbitrary file. `*TRELLIS` is not a public command. | `modules/RicochetCommands.bas64` descriptors; `is_registered_rust_command` and manifest validation in `src/ricochet.rs`; `execute_cli_command` in `src/swi.rs`. `TRELLIS-MANIFEST` survives only as a pre-release manifest decoder compatibility spelling, not CLI surface. |
| Identity and authority | Registry rows contain bounded display/handler metadata, not source text or host pointers. The registry mechanism is granted only to the trusted command owner. Handler invocation passes the original `&mut Task`; nested `OS_CLI` calls preserve that task principal, while source inspection and module/configuration mutation still use their separate `SourceRead`, `ModuleManagement`, and `ConfigurationWrite` checks. | Foundation grants in `src/boot.rs`; invocation in `SwiDispatcher::invoke_registered_command`; protected operation checks in their shared service handlers and `tests/ricochet_command_help.rs`/authorization suites. Registry metadata is intentionally visible to the command owner; this does not grant guest source-read or mutation rights. |
| Bounds and scratch | OS_CLI has the checked 256-byte input contract. Command metadata is bounded at parse/manifest validation. Command dispatch and Rust bridge operands use task-local command scratch, released by the OS_CLI dispatcher on success and error; file-command two-operand staging is within the 4 KiB area. | `OS_CLI` manifest declaration and scratch helpers in `modules/RicochetCommands.bas64`; `GuestMemory::acquire_command_scratch` and dispatcher cleanup in `src/memory.rs` / `src/swi.rs`. The RENAME bridge success and caller sentinel checks are in the public command/help fixture. |
| Public name and routing | `RicochetCommands` is the public owner name, while functional `*INSPECT` remains available. Classic supported names route through this registry. Native-only ROM/RMA operations are explicit unsupported entries/diagnostics, not aliases for unrelated operations. | Manifest and descriptors at the top of `modules/RicochetCommands.bas64`; command/help integration fixture. Hosted module commands remain narrower than native `&FFA` modules. |

## Current test status

I independently ran:

```text
ACORN_CONFIG_PATH=/private/tmp/acorn-command-help-audit-<unique>.configure \
  cargo test --no-default-features --test ricochet_command_help -- --nocapture
```

The initial independent run reached the public `--stdio` sequence and exposed
lost first command letters (`Bad command: ELP` and `Bad command: UIT`). The
implementation traced this to interpreter key polling consuming bytes queued
for the next OS_CLI line while BASIC64 formatted Help; suppression only inside
the selected handler was too short because the enclosing OS_CLI PROC still had
steps to execute. The corrected path keeps polling suppressed through the
whole module-owned OS_CLI SWI frame, while Rust bridges can re-enable it for an
interactive guest program. The sequential process regression now passes.

Independent rerun on the corrected snapshot:

```text
RICOCHET_CONFIG_PATH=/private/tmp/ricochet-command-help-audit-<unique>.configure \
RICOCHET_DEMO_VOLUME=/private/tmp/ricochet-command-help-audit-volume-<unique> \
  cargo test --no-default-features --test ricochet_command_help -- --nocapture
```

Result: `help_and_command_registry_are_live_module_owned_and_read_only` passed
(1 passed, 0 failed). This run includes the real stdio sequence, direct OS_CLI
Help and matching checks, guest load/replacement/unload, command-only owner,
authority checks, and scratch/sentinel assertions. It is a focused integration
result, not a full-suite result.

Other audit findings fixed before this passing run: two-operand bridge staging
was moved inside the 4 KiB task scratch allocation; case-insensitive module
Help now uses the canonical installed title after converting the
ModuleLookup/ModuleInfo index correctly; and the stdio poll suppression now
covers the complete OS_CLI frame. No unresolved defect in this audited command
registry/help boundary was reproduced by the focused test.

## Deliberate compatibility gaps

- Help streams output without a terminal full-screen pause or Task-window
  scroll implementation. The User Guide treats those as different UI
  behaviors; the hosted test currently proves text content and routing, not
  pager interaction or window scrolling.
- Module lookup order is the hosted case-folded title order, not native dynamic
  module-list order. The hosted `*Modules`, Help listing, and execution all use
  that same order, so the first-match rule is internally coherent; command
  priority may differ from a RISC OS installation whose module load order
  differs.
- Native Alias$ type-2 macros, general GSTrans, redirection, `/Run`, filing-
  system context overrides, unknown-command service callbacks, and normal
  `*Run` path fallback remain unsupported. A leading `%` bypasses the hosted
  alias resolver once; see the linked alias audit for its bounds and deviations.
- Obey is a bounded guest-file subset with path/line provenance, fail-stop
  behavior, nested frame cleanup, and one-pass parameters; see the Obey audit
  chain for details. The separate bounded `*EXEC` input stream has its own
  implementation and audit; automatic script launch and general environment
  expansion remain future work.
- Command-bearing metadata is a backward-readable extension in the current
  manifest-v1 decoder: old manifests without `command.count` decode to no
  commands. Older strict v1 readers reject the new command fields, so this is
  not forward compatibility with older runtime binaries.
- This verifies the module-owned command/help slice, not broad Phase 6 UI
  introspection parity or a general RISC OS command environment.

## Audit conclusion

The registry ownership and lifecycle design is implemented in the actual
execution path, and the checked metadata/private-handler/explicit-bridge
boundary is supported by source inspection and the passing focused public
integration. No defect remains open in this audited slice. The result is still
bounded: the deliberate pager, native module-order, command-environment, and
old-reader manifest limits above remain, and this is not full RISC OS CLI or
Phase 6 UI parity.
