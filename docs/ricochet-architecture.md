# Ricochet: Live System Architecture

## Status

This document records the architectural direction for **Ricochet**, the project
previously described under the working name Ricochet. It is the basis for
future work packages.

The dependency-ordered implementation plan is maintained in
[`ricochet-work-packages.md`](ricochet-work-packages.md).

The mission, module ownership of SWIs, absence of native public SWI handlers,
and bootstrap boundary described here are firm design decisions. The complete
live-system experience, language extensions, persistence model, optimisation
strategy, and desktop projections remain staged design work.

### Implementation checkpoint — 2026-10-01, Phases 0–4 plus partial WP5.1/WP5.3/WP5.4/WP5.5/WP5.6

The initial Console SWIs (`OS_ReadLine`, `OS_WriteC`, `OS_WriteS`, `OS_Write0`,
`OS_NewLine`, and `OS_ReadC`) remain versioned entry cells implemented by
interpreted definitions in `modules/Console.bas64`. Phase 4 assembles a
deterministic v1 boot capsule from the visible source and canonical manifests
for fifteen foundation/command modules: System, ModuleManager, Error,
TaskManager, Memory, Console, RicochetCommands, Boot, Mos, FileSwitch,
Graphics, ColourTrans, DesktopServices, DisplayManager, and Wimp. The executable has no hand-maintained capsule
archive: `include_str!` rebuilds its source inputs and `--write-boot-capsule`
emits the same validated bytes on demand. Integrity uses CRC-32 for corruption
detection, not cryptographic authentication; embedded sources are the trust
root, while an alternate capsule is explicitly selected and receives only the
host's fixed module/capability grants.

Native initialization starts with an empty public SWI registry, validates and
links modules directly from capsule bytes without FileSwitch or public SWIs,
and publishes the complete initial export set in one transaction. Start hooks
run afterwards. Any start failure discards the entire initial namespace and
module workspaces before exposing the restricted recovery interface. Recovery
uses native host I/O/display mechanisms only; it reports stage, module,
definition, structured cause and ABI details, and accepts only retry, a selected
capsule path, or exit. It does not provide normal CLI, guest files, or SWIs.

`modules/Boot.bas64` owns the `Language 0` versus `Language 3` decision. It reads
the persisted value through the qualified BASIC64 `System.ReadStartupLanguage`
definition and requests the MOS prompt or desktop through qualified System
handoff definitions. `System.bas64` alone imports those protected settings and
handoff primitives. `Runtime::run` only consumes the selected request; it does
not interpret the saved language value. `RicochetCommands.bas64` owns OS_CLI
(`&05`) dispatch plus read-only `*INSPECT`, the supported classic module
commands, `*CONFIGURE`, `*STATUS`, and module-owned `*HELP`. A live registry of
`@COMMAND` descriptors is the sole command inventory for execution and Help.
Transitional Rust command semantics are reachable only through explicit,
closed-allowlist bridge rows in `RicochetCommands`; a registry miss is a
BASIC64 `Bad command`, never a catch-all Rust fallback. Registry order is
alphabetic by case-folded module title, then source declaration order.
Execution selects the first exact or final-dot prefix match; Help displays all
matches for a prefix. Module publication, replacement and removal update rows
atomically with the owning module. Help follows RISC OS topics (`Commands`,
`FileCommands`, `Modules`, and `Syntax`), streams output without a terminal
pager, and reports actual versions without inventing dates or addresses.
Configuration policy remains in BASIC64, with checked persistence as a host
mechanism. `FileSwitch.bas64` owns bounded `OS_File` reasons 0–12, 16–18, and
255, `OS_Find`, `OS_BGet`, `OS_BPut`, `OS_Args`, `OS_GBPB` reasons 1–10, and
bounded `OS_FSControl` reasons.
Its OS_File routing selects File$Path, R4 path-list, path-variable, or direct
search by reason and iterates candidates in BASIC64. Rust provides bounded
runtime-variable/path-string retrieval, checked candidate construction and
guest-sandbox lookup/load mechanisms, task-local channels, catalogue snapshots,
transfers, and checked caller buffers. String/LiteralString variables only are
supported; Run$Path, macro expansion, wildcards, and general path search remain
deferred. OS_FSControl owns bounded reasons 0, 1, 5–9, 11, 13, 14, 18, 19, 22,
25, 31, 33, 37, 39, 40, 43–45, and 50. Reason 11 reports the supported
temporary HostFS selection and prior selector; unsupported special fields fail
atomically. Reason 37 supports bounded R3 path-variable and R4 ordered path-list
lookup with qualified-path bypass; general GSTrans/macros remain unsupported.
Reasons 7/8 use one library-relative effective path for title and entries.
Reason 50 changes the shared HostFS volume label. Other FileSwitch reasons, native BASIC file statements,
and full compatibility remain deferred, so WP5.4 is still incomplete.

The partial WP5.5 graphics ownership slice adds `Graphics.bas64` to the
foundation capsule. It owns public `OS_Plot` (`&45`) plot-byte/coordinate
adaptation and `OS_ReadPoint` (`&32`) profile/result policy. Protected raster
mechanisms resolve the original caller's active Wimp redraw context or bounded
task-default raster and retain the CPU-authoritative plot plus
display-event/batch publication path. The
existing VDU byte-stream parser, raster algorithms, and renderer remain Rust
mechanisms; indexed-colour fidelity and full plot-code compatibility remain
open. `ColourTrans.bas64` also owns the existing three name-only ColourTrans
services without inventing numeric SWI identities. It implements the hosted
HSV conversion and SetGCOL policy over the same caller-raster mechanism;
WritePalette remains an explicit no-op. Native numeric ColourTrans ABI and
palette mutation remain unsupported. This is not completion of WP5.5 or Phase 5.

The same command module now owns the bounded guest system-variable policy:
`*SET <name> [value]`, `*SHOW [pattern]`, and `*UNSET <pattern>` use the
module-owned `OS_ReadVarVal` (`&23`) and `OS_SetVarVal` (`&24`) exports. Rust
provides one runtime-scoped store and checked logical-memory/context
mechanisms. Type 0 immediately expands exact `<name>` references, outer and
doubled quotes, and the printable escapes `|<`, `|>`, `||`, `|"`; type 4
remains raw. Substituted bytes are not rescanned. Numeric angle operands,
wildcards in references, control-code escapes, malformed syntax, and missing
variables fail before mutation. The PRM source documents this substitution
and escape behavior; the hosted subset does not expose an `OS_GSTrans`
register contract. Values are guest-owned, session-local, and never imported
from the host process environment. Names are visible ASCII up to 32 bytes,
values and expanded results are UTF-8 up to 256 bytes, and the store is bounded
to 128 entries and 32 KiB aggregate data. Lookups are case-insensitive and
preserve the first display spelling; enumeration order is deterministic. The
bounded `*`/`#` wildcard matcher allows SET only if exactly one existing
variable matches, while UNSET removes all matches. `OS_ReadVarVal` uses checked
caller-owned strings/buffers and task-owned dynamic name/context areas; errors
release the matching continuation context. Variable mutation has its own
`SystemVariableWrite` Task right, deliberately granted to the trusted
interactive MOS Task and separate from configuration, source-read, and
module-management rights. Ordinary tasks can read this guest store but cannot
read host environment variables. The initial `*OBEY <guest-path>` subset is
also available through the active command registry. BASIC64 owns line
classification and dispatch; bounded Rust primitives read checked guest files
and maintain task-local nested source frames. It supports LF, CRLF, and CR,
blank lines, `|` comments after leading spaces/tabs, and a final line without a
terminator. It stops on the first command error and reports guest path and line;
prior command effects are retained. QUIT unwinds without reading later lines.
The hosted limits are 65,536 bytes across active sources, 255 bytes per command
line, 4,096 physical lines per nested session, and eight active nested sources.
This is a deliberate hosted subset, not full PRM Obey compatibility. It
supports bounded one-pass `%0`–`%9`, `%*n`, and `%%` substitution in BASIC64:
arguments are separated by spaces outside double quotes, quote spelling is
preserved, missing arguments become empty strings, and `%*n` preserves the raw
suffix from argument n. Unmatched quotes and incomplete `%*` forms fail before
that line is dispatched; expanded commands over 255 UTF-8 bytes fail without
truncation. While a script frame is active, task-local read-only `Obey$Dir` is
exposed through OS_ReadVarVal and type-0 expansion. It contains the stable
resolved guest parent path (not a host path), shadows any stored value only
during the active frame, and restores naturally across nesting, errors, and
QUIT. This intentionally differs from PRM's invocation-text parent fragment.
Bounded `Alias$<command>` variables are checked before the registry, and a
leading `%` bypasses alias lookup once. Existing String and LiteralString
variables are accepted; aliases can shadow commands, use final-dot unique
prefixes, and recursively expand one command with `%0`–`%9`, `%*n`, and `%%`.
The hosted cap is eight alias expansions, 255 UTF-8 bytes per expanded line,
and 2,048 cumulative expansion bytes. These are static string aliases, not
native Type-2 macro variables; command chaining, redirection, pipelines and
general GSTrans remain unsupported. `*SHOW Alias$*` inspects live values and
`*HELP ALIASES` explains the feature; aliases are not registry entries and do
not add caller rights. PRM `-v`/`-c`, general command substitution, and
automatic filetype or `!Boot` execution remain deferred. Bounded `*EXEC`
installs one task-scoped guest input source consumed through `OS_ReadC`,
`OS_ReadLine`, and BASIC input; it replaces atomically, bare `*EXEC` closes,
and EOF returns reads to the queued/host stream. The source is preflighted as
UTF-8 text, limited to 65,536 bytes, 255 bytes per line, and 4,096 lines, and
does not expose native `OS_Byte 198` handle control. See the Exec audit.

`Mos.bas64` owns public `OS_Byte` (`&06`) and `OS_Word` (`&07`) reason
selection for the hosted subset. It calls narrowly typed input, clock, and
checked five-byte caller-memory primitives; Rust no longer has numeric
fallback implementations for these SWIs. Supported byte reasons are 21
(`X=0`), 129 (`Y<128`), and 138 (`X=0`); supported word reasons 1-4 read or
write the system/interval 40-bit clocks. Other selectors fail explicitly.
`*FX` routes its numeric form through the same public `OS_Byte` dispatch.
This is not native hardware/timer compatibility and does not add reason 198.

Configuration-file recovery is a separate post-foundation path, not native
capsule recovery. The typed store bounds reads at 64 KiB and classifies
malformed/truncated, unsupported version/schema, invalid UTF-8, unreadable, and
oversized files. `System.ReadStartupLanguage` returns safe effective defaults
and a recovery code instead of failing `Boot.Start`; BASIC64 Boot reports the
cause without exposing a host path, then follows the normal MOS/desktop startup
policy. `*STATUS` reports effective values plus the latched cause. The file is
not changed during boot or reads. The first authorized successful setting
write or `*CONFIGURE DEFAULTS` creates a unique sibling recovery copy of any
recoverable source (oversized inputs are copied by streaming) and atomically
writes canonical v3 settings. A failed backup or save leaves active recovery
latched and the original file unchanged. NotFound is ordinary first-run
defaults; non-NotFound I/O failures are shown as unreadable storage. This path
does not expand the native capsule recovery prompt or expose guest SWIs before
the foundation is ready.

The current v3 configuration profile has six public keys: `Language`,
`WimpMode` (`Mode` alias), `BASICMode`, `BASICProfile`, `BASICTarget`, and
`BASICEngine`. `Language` retains the standard numeric form but this host
accepts only module 0 (MOS prompt) and 3 (desktop). `WimpMode` accepts `Auto`
or `X<width> Y<height> C/G<depth>` for the six documented hosted sizes and
eight supported depth tokens; numeric monitor mode IDs, EX/EY, and refresh
selectors have no hosted monitor table. WimpMode alone controls both
resolution and palette: Auto follows host content size and selects full-colour
C16M/Rgb888. Previous `DisplayResolution`/`DisplayColour` pairs are migrated
when loading old files, with Window becoming Auto/full-colour; explicit v2
WimpMode takes precedence over the retired `RicochetOutputProfile` row.
`WindowFurniture` is discarded. These old keys are rejected by public
commands and full configuration resets. The renderer has one flat
appearance, and `WindowFurnitureLayout` is only a geometry/hit-test type.

The Phase 4 foundation was the original seven-module set; the current capsule
adds `RicochetCommands` as the first bounded WP5.3 command module and `Mos` for
the bounded `OS_Byte`/`OS_Word` policy. Console owns the six
migrated character services; `Error` owns `OS_GenerateError`; `Memory` owns
`OS_ChangeDynamicArea` and `OS_DynamicArea`; `ModuleManager` owns
`Ricochet_ModuleInfo`, `OS_Module`, `Ricochet_ModuleLookup`, and `Ricochet_SwiInfo`; and
`TaskManager` owns `Ricochet_TaskInfo`. These are executable definitions in the
capsule, with Rust providing only their capability-gated mechanisms. The
current Phase 5/WP5.1 checkpoint adds a bounded post-boot `OS_Module` Load
(reason 1) and Delete (reason 4) subset over visible BASIC64 `&064` source,
safe manifest-derived identity queries, and a common X-form error block/V
transport. A same-title guest Load supports a narrow compatible-immediate
replacement: it preserves module and entry-cell IDs, retains the workspace
when persistent declarations and all named type layouts match, and advances
the full exported SWI set in one registry transaction. Active leases keep the
old source generation; subsequent calls use the new source. Case-only title
changes are normalized to the installed display spelling. Candidate Start,
old Quiesce, and Finalise hooks are not run during this in-place swap. Manifest,
dependency, capability, exported SWI/PROC/FN signature, lifecycle-contract, or
state-schema changes are rejected without changing the active module; foundation
modules are protected. PRM reason 1 instead kills all same-title instantiations
before initializing the new image, so this is a deliberate, rollback-safe
hosted deviation. `System` now also owns PRM-numbered `OS_SWINumberToString` (`&38`),
`OS_SWINumberFromString` (`&39`), and `OS_ReadMonotonicTime` (`&42`) through
capability-gated Rust mechanisms. The conversion services use active manifest
identities only, preserve the X bit and exact-case lookup rule, and read/write
checked caller-task buffers bounded to 128 bytes. Transitional Rust-only and
unknown services, `OS_WriteI` values, and numeric module-chunk aliases have no
identity mapping and return structured errors. The hosted monotonic value is
32-bit centiseconds since runtime startup. `ModuleManager` also owns the
read-only `Ricochet_ModuleExport` (`&4FF14`) and `Ricochet_DefinitionSource`
(`&4FF15`) queries used by the commands and intended future browser. Source is
read from retained active BASIC64 definitions using checked caller memory and
bounded chunks; source output escapes display control bytes. Active
module/version/state and exported SWI identities are explicitly public
metadata; they describe published service names, not source or task state.
The named-only project services `RICOCHET_DESKTOP` and `RICOCHET_DISPLAY`
are also module-owned: a closed dispatcher map invokes
`DesktopServices.bas64::DESKTOPSERVICE` and
`DisplayManager.bas64::DISPLAYSERVICE`, without assigning numeric SWI IDs.
Their BASIC64 definitions own action/version/enum/register policy. Rust
provides bounded HostFS catalogue, Wimp menu, and display query/persistence
mechanisms. Display apply checks the original caller's `ConfigurationWrite`
right before any settings effect; module grants do not confer that right.
`Wimp.bas64` now owns the lifecycle trio and the bounded OpenWindow,
CloseWindow, GetWindowState, and SetExtent SWIs. BASIC64 decodes caller blocks
and applies the supported stack policy; narrow Rust methods recheck task/window
ownership and geometry while committing synchronized state transitions. The
other Wimp entries remain transitional hosted routes, so WP5.6 remains partial.
Definition-source queries require the original caller Task's `SourceRead`
authority, and OS_Module Load/Replace/Delete requires the separately scoped
`ModuleManagement` authority. ModuleManager BASIC64 asks for each right before
the operation, and Rust rechecks it at the shared service handler using the
same caller Task passed through nested SWI/provider calls. A privileged
ModuleManager provider capability never upgrades that requestor. Host
bootstrap grants are private fields on the Task object, not derived from its
numeric ID: `Task::trusted_mos_session` is the explicit interactive MOS
bootstrap; source-only and manager-only host profiles are distinct; spawned
desktop tasks use ordinary `Task::new` and do not inherit. Code executing in a
trusted session shares that task-scoped authority; stronger guest-program
isolation requires a separate ordinary task. Grants last for the Task
object's lifetime and are revoked by host task teardown/replacement; there is
no guest-visible grant/revoke API. `*INSPECT` and `*Modules` are read-only;
`*RMLoad`, `*RMRun`, `*RMKill`, and an RMEnsure fallback that invokes a
mutation reach the separately authorized `OS_Module` interface.

The command module exposes read-only `*INSPECT MODULES [filter]`,
`*INSPECT MODULE <title>`, `*INSPECT SWI <name|number>`, and
`*INSPECT DEFINITION <module>/<definition> [byte-offset]`; `SOURCE` is a
read-only alias. These call the same structured ModuleManager query SWIs
reserved for future UI parity. Selectors use `FN:` to disambiguate functions;
only active definitions/current source are available. CLI byte offsets are
strict decimal values in the nonnegative signed-32-bit range.

The implemented classic mutation/list subset is `*Modules`, `*RMLoad`,
`*RMRun`, `*RMKill`, and conditional `*RMEnsure`. `*Modules` reports hosted
logical module titles, semantic versions, and active state; it does not invent
historical memory/workspace addresses or `%instance` records. RMLoad/RMRun
accept one RISC OS guest path mapped to visible BASIC64 `&064` source; optional
module initialization strings are rejected. RMRun currently equals RMLoad
because hosted modules have no separate application entry. RMKill accepts a
full title and does not support `%instantiation` selection. RMEnsure compares
numeric `major.minor[.patch]` components, is a no-op when the installed
version is equal/newer, and dispatches the complete bounded command tail only
when missing/older; an unsatisfied check without a tail raises a CLI error.
Same-title Load uses Ricochet' compatible atomic guest-replacement rules, which
are deliberately rollback-safe and differ from PRM's destructive native `&FFA`
replacement lifecycle.

The supported public BASIC file routes are `*BASIC <guest-path>` and
`*RUN <guest-path>`; both run source or tokenized BASIC directly through the
configured execution engine. `*BASIC64` retains its mode/text launch options.
The saved `BASICEngine` setting selects Interpreter, Hybrid, or Strict for the
next file/desktop launch. Separate load-then-run commands, their per-task
tokenized-program cache, and one-shot JIT command overrides were removed as
redundant public surface. See the bounded command/script milestone audit for
the migration and explicit losses.

Known classic names `*RMReInit`, `*RMInsert`, `*RMTidy`, `*RMClear`,
`*RMFaster`, `*ROMModules`, and `*Unplug` return explicit unsupported
diagnostics because this host has no ROM/RMA/unplug state model. This is not a
claim that these operations are aliases for module load/delete. Abbreviations
come from a fixed BASIC64 command table (including the unique `*M.` and `*I.`
prefixes), not the PRM's dynamically assembled module alias order; collisions
such as `*INSPECT MOD.` are reported as ambiguous while exact `MODULE.` selects
detail. OS_CLI strips repeated leading `*`/whitespace, accepts PRM NUL/LF/CR
terminators, and preserves R0.
Transitional Rust command handlers are explicit registry bridge descriptors,
not a hidden fallback. Broader MOS command policy and other WP5.1 compatibility remain open. The
native loader still installs the initial namespace directly and never calls
public `OS_Module`.

State migration, quiescent/restart-required replacement, OS_Module parameters,
broader System query compatibility, task creation/scheduling, OS
error-vector handling, and the remaining transitional Rust SWI semantics are
still Phase 5 work. This checkpoint does not mark WP5.1 complete. No public SWI
is installed as a hidden bootstrap service. `Boot`
owns Language 0/3 policy through qualified System imports.
`Host.Graphics.AcceptByte` contains the hosted VDU byte-stream policy while
`Host.Console.WriteByte` remains raw host output. Capsule contracts, RISC OS
deviations, and hosted memory limits are recorded in
[`ricochet-boot-capsule.md`](ricochet-boot-capsule.md).
See [`ricochet-boot-capsule.md`](ricochet-boot-capsule.md) and
[`ricochet-work-packages.md`](ricochet-work-packages.md) for the wire contract and
package-level status.

System Profile 0.1 now has a separate, source-located typed high-level IR whose
operations represent the supported expressions/statements without retaining
parser AST payload. Interpreted module calls prepare that representation and
reconstruct the common reference-interpreter program from its IR. The slice
also executes linked cross-module PROC/FN calls, checks managed resource rights
at use, scopes typed local read-only bindings in PROC/FN calls, and preserves
exact explicitly typed `INT64`/`UINT64` operations. Hybrid/Strict JIT and AOT
requests use the same admission boundary and reject native System Profile
lowering explicitly. The IR is not serialized module bytecode or a native
module compiler; the v1 boot capsule stores visible source and canonical
manifests. Lifecycle workspace writes are transactional: failed `Quiesce`
restores the prior workspace and `Active` admission; failed `Finalise` restores
workspace state but leaves the module safely quiesced, exports inaccessible and
source/workspace retained for retry. Irreversible host effects performed by a
hook primitive remain outside that transaction and must be deferred by hooks.

## Mission

> **Ricochet makes the computer understandable, programmable and malleable by
> the person using it—through a modern, live and
> inspectable RISC OS environment.**

## Architectural charter

> **Ricochet is a RISC OS-compatible hosted environment in which BASIC64
> definitions, system services, tasks and resources have stable logical
> identities and inspectable relationships. Selected BASIC64 behaviour can be
> replaced live through versioned definitions, while Rust enforces isolation,
> capabilities and machine-facing mechanisms.**

Ricochet is a new live operating environment with a deliberately compatible
RISC OS programming and user model. It preserves useful public contracts and
source behaviour where feasible without retaining historical implementation
constraints. Backwards compatibility with the binary implementation or module
ABI of historical RISC OS is not a requirement.

## Central principle

> **Nothing running in the computer should unnecessarily lose its meaning.**

Source code is not merely an input from which an opaque program is
manufactured. It is one live representation of a program, connected to its
parsed form, executable form, state, resources, callers, active invocations,
and provenance.

A procedure can therefore retain relationships such as:

```text
SaveDocument
  source        -> editable BASIC64
  syntax        -> parsed representation
  IR/bytecode   -> portable executable form
  native code   -> current compiled generation, if any
  references    -> FileSystem, CurrentDocument
  callers       -> SaveCommand, Autosave
  executions    -> current statistics
  active calls  -> current invocations
```

Compilation is a derived representation, not a destructive transition. The
editable source remains the source of truth. Derived code is rebuildable and
retains enough source and dependency information for inspection, invalidation,
and safe replacement.

This principle does not require every internal Rust value to become a universal
meta-object. Every important system entity instead has a stable, inspectable
description connected to its live implementation. Efficient internal state may
remain in Rust while exposing logical identity, relationships, and controlled
operations to the live system.

## System shape

```text
User programs and applications                         BASIC64
----------------------------------------------------------------
Desktop, Filer, tools, policy and system browser       BASIC64
----------------------------------------------------------------
Live modules, SWIs, services and versioned definitions BASIC64
----------------------------------------------------------------
BASIC64 interpreter, IR, JIT/AOT and live registry     Rust
Module lifecycle, SWI dispatch and capabilities        Rust
Logical memory, tasks and protected primitives         Rust
----------------------------------------------------------------
Host operating system, rendering libraries and devices
```

Rust supplies mechanisms and trust boundaries. BASIC64 owns the public system
service surface and the behaviour that a user may reasonably inspect or alter.
The Rust substrate should be small in authority and policy, not artificially
minimised by line count.

Rust remains responsible for mechanisms whose corruption could violate another
task's authority or make the live system unrecoverable, including:

- BASIC64 execution and runtime support;
- logical memory allocation, translation, sharing, and protection;
- task identity, scheduling mechanisms, and failure containment;
- module loading, validation, lifecycle, and version retirement;
- SWI dispatch mechanics and caller-context construction;
- capability validation;
- JIT infrastructure and executable-memory management;
- host integration, primitive I/O, rendering, and event delivery;
- integrity checking and emergency boot diagnostics.

Higher-level policy belongs in BASIC64 modules wherever practical.

## BASIC64

BASIC64 remains recognisably descended from BBC BASIC and retains an explicit
BBC BASIC V/VI compatibility personality. Its native system-language profile
adds the bounded facilities specified below and in the System Profile document.
Extensions are additive and explicitly selected; they must not silently
reinterpret compatible BBC BASIC source.

The language should grow from concrete requirements imposed by the next system
layer. Ricochet does not assume that every operation requires universal
Smalltalk-style late binding. Static or guarded calls are appropriate where
they preserve live replacement and inspectability.

The interpreter remains the reference and universal execution path. Portable
IR can feed an interpreter, a tiered JIT, and later AOT compilation. Optimised
code must preserve caller identity, logical memory checks, capabilities, source
locations, dependency information, invalidation, and deoptimisation.

Before executable Ricochet modules are built, the native language will gain a
deliberately bounded **BASIC64 System Profile 0.1**. Its agreed facilities are
modules and visibility, named records, enums and flags, typed definitions,
structured errors, opaque handles, read-only bindings, SWI/primitive metadata,
and a strict distinction between managed references and logical addresses.
Structures, resource identities, and behavioural modules remain separate
concepts. Classes, inheritance, generics, universal message dispatch, macros,
and async syntax are deferred until concrete system requirements justify them.
See [`basic64-system-profile.md`](basic64-system-profile.md).

## Live entities and relationships

The live registry gives stable logical identities to important entities such
as:

- modules and definition generations;
- procedures and functions;
- tasks and active calls;
- logical address spaces and memory regions;
- files and file channels;
- windows and graphical surfaces;
- timers, devices, fonts, and network resources;
- SWIs and their owning module definitions.

These identities are not unchecked host pointers. They are runtime-managed,
caller-scoped references with explicit rights and lifetimes.

Inspection and authority are separate:

```text
identity              know that an entity exists
metadata visibility   see selected description and provenance
observation            inspect changing state
authority              operate on or transfer the entity
modification           replace its behaviour
```

A globally navigable live graph therefore does not imply a global authority
graph. An inspector may reveal that a task owns a file channel without granting
the inspecting code permission to read, write, or close it.

Runtime provenance should be bounded and purposeful. Stable ownership,
creation, dependency, and current-use relationships are valuable. Unbounded
retention of every historical event is not a requirement.

## SWIs and modules

### Firm invariant

> **Every Ricochet SWI is exported by a module and implemented by a versioned
> BASIC64 definition.**

Rust hard-codes no public SWI names, numbers, or implementations. The Rust
substrate supplies the dispatcher and protected primitives, but it does not own
the public SWI namespace.

A BASIC64 SWI definition may:

- implement the complete service in BASIC64;
- delegate to another module definition;
- alias another module's SWI implementation; or
- validate and adapt a public contract before invoking a protected Rust
  primitive.

```text
Application
    -> public SWI
    -> owning BASIC64 module definition
    -> BASIC64 service, alias, or protected primitive
    -> Rust / host / hardware where necessary
```

This rule ensures that inspection of any SWI reaches a named module and an
editable, versioned BASIC64 definition, even when its final operation delegates
to Rust.

### SWIs are not primitives

SWIs are the public service interface. Primitives are a private, typed substrate
interface used by authorised modules. Rust primitives do not occupy the public
SWI namespace and are not callable merely by knowing a number or address.

Primitive imports are declared by module manifests and resolved by the trusted
loader. Examples include logical memory operations, task mechanisms, host file
access, rendering submission, console I/O, monotonic time, and definition
publication. The loader grants only the primitives allowed by a module's
capabilities.

### Efficient dispatch

SWI numbers resolve through stable, versioned entry cells:

```text
SWI number
    -> indexed registry entry
    -> versioned entry cell
    -> current BASIC64 definition generation
    -> optional protected primitive
```

There is no source parsing or name lookup on the hot path. A trivial BASIC64
wrapper can compile to capability and argument checks, a direct primitive call,
and result adaptation. Aliases can resolve to the same entry cell. Optimised
callers may cache targets behind version guards; replacement invalidates the
guard and returns subsequent calls to the current generation.

### Module model

A Ricochet module is a live BASIC64 package that may contain:

- SWI definitions and aliases;
- service-call and event handlers;
- commands;
- versioned procedures and functions;
- private workspace and persistent service state;
- initialisation, upgrade, quiescence, and finalisation definitions;
- capability requirements;
- documentation and introspection metadata;
- declared Rust primitive dependencies.

Module manifests identify every SWI export by module identity, module version,
SWI number and name, definition identity, argument and result contract,
capability requirements, and replacement policy. Arbitrary code cannot install
an anonymous host function directly into the SWI table.

This preserves the useful RISC OS module concept without retaining historical
module headers, ARM branch tables, relocation conventions, shared-address-space
entry points, raw host pointers, or privileged native module code.

### Manifest schema 1 checkpoint

The current native model serializes a resolved module manifest as deterministic
UTF-8 text headed by `RICOCHET-MANIFEST<TAB>1`. Each subsequent row is
`key<TAB>value`; strings are percent-escaped, collections use indexed fields
plus explicit counts, and fields have one canonical ordering. Schema 1 records:

- module name, semantic version, language and target profiles;
- source path and change-detection hash;
- dependency names/minimum versions and imported/exported BASIC64 symbols;
- primitive imports and requested capabilities;
- lifecycle procedure names and the replacement policy;
- each SWI number/name/definition, typed register directions, caller-memory
  pointer/direction/size, PC/carry contract, blocking/re-entrancy properties,
  and failure-transport label.

The decoder rejects unknown, duplicate, missing, malformed, and trailing
fields. Validation also rejects invalid module/source paths, unsupported target
profiles, duplicate/self dependencies, unresolved symbol visibility,
duplicate SWI ownership inside the manifest, invalid register widths or
directions, ambiguous PC declarations, unsafe memory pointer contracts, and
primitive imports not covered by a declared request and host grant. The full
source metadata spelling and example are in
[`basic64-system-profile.md`](basic64-system-profile.md); the SWI surface is
tracked in [`ricochet-swi-inventory.yaml`](ricochet-swi-inventory.yaml).

Replacement metadata values are `IMMEDIATE`, `QUIESCENT`, `MIGRATING`, and
`RESTART`. Compatible-immediate replacement is implemented for individual
trusted-host definition updates and, through guest `OS_Module` reason 1, for a
bounded whole-source module class. The guest path requires unchanged manifest
identity/dependencies/capabilities/lifecycle/public contracts, matching
exported PROC/FN signatures, and identical persistent state plus the full named
type table; it shares the existing workspace and does not run lifecycle hooks.
The registry swaps all module SWI cells while exclusively borrowed, preserving
cell IDs and active old leases. Candidate rejection occurs before publication.
The other values are reserved policy labels, not working state-migration or
restart workflows. Schema 1 and source fingerprints are deterministic and
serializable but are not signed or cryptographically authenticated; FNV-1a is
only a cache/change detector.

### Live replacement

New calls enter the current definition generation. Calls already executing may
finish against the retired generation. Retired code and state remain alive
until no active invocation or retained reference requires them.

Replacement falls into explicit classes:

- immediate replacement for compatible definitions or state-preserving module
  generations whose workspace schema is unchanged;
- quiescent replacement after active calls finish;
- migrating replacement with a checked state-upgrade definition;
- restart-required replacement when foundational invariants change.

Only the compatible-immediate path is presently implemented. Guest source
replacement is further restricted to unchanged dependencies, authority and
lifecycle metadata/contracts, with all public SWIs committed together and the
existing workspace shared. It rejects schema/API changes rather than attempting
Phase 7 state migration. Failure validation leaves the installed module
unchanged. Live modification is not permission to corrupt the system silently.

## Bootstrap architecture

### Decision

No public SWI needs to be hard-coded in Rust, including `OS_WriteC`,
`OS_Module`, or error services. Before the first modules become active, there
is no SWI environment.

The Rust dispatcher understands the shape of a call—number, caller context,
registers, error transport, and versioned target—but it does not know the
meaning of a public SWI number.

### Boot sequence

```text
0. Native host entry
        -> 1. Construct substrate runtime
        -> 2. Load trusted boot capsule
        -> 3. Validate and link foundation modules
        -> 4. Publish initial SWI exports atomically
        -> 5. Start the module system
        -> 6. Load normal system modules
        -> 7. Start configured command line or desktop
```

#### 0. Native host entry

Rust initialises process state, the root logical address space, BASIC64
execution, capability enforcement, the empty module registry, the empty SWI
table, emergency diagnostics, and access to the boot capsule.

No filing system, command line, desktop, or public service exists yet.

#### 1. Construct the substrate runtime

Rust creates the mechanisms needed to instantiate BASIC64 safely: logical
memory, task contexts, module instances, definition generations, versioned SWI
entry cells, structured internal errors, and the protected primitive gateway.

#### 2. Load the boot capsule

The executable embeds, or is distributed with, a trusted boot capsule. It is a
ROM-equivalent package rather than a mounted filing system. Rust accesses it
directly without using `OS_File` or another SWI.

The target capsule can contain visible BASIC64 source or rebuildable portable
IR, module manifests, dependencies, primitive imports, SWI exports, integrity
metadata, and optionally discardable native-code caches. The current v1 capsule
stores visible source, canonical schema-1 manifests, explicit host grants,
runtime ABI, and a CRC-32 integrity check; it contains no native-code cache.

The target architecture anticipates small foundation modules such as:

```text
System
ModuleManager
Error
TaskManager
Memory
Console
Boot
```

The current capsule contains the original seven Phase 4 foundation modules,
plus `RicochetCommands` and the MOS policy owner: `System`, `ModuleManager`,
`Error`, `TaskManager`, `Memory`, `Console`, `Boot`, `RicochetCommands`, and
`Mos`. The first usable service
slices are source-defined and manifest-owned;
their precise boundaries and remaining breadth are recorded in
[`ricochet-boot-capsule.md`](ricochet-boot-capsule.md).

#### 3. Validate and link foundation modules

The loader validates each package, resolves authorised primitive imports,
allocates module workspace, constructs versioned definitions, creates
unpublished SWI entry cells, and resolves dependencies.

Loading and starting are separate phases. Load/link code may use only its
declared primitives. It may not assume that public SWIs are already available.
If any foundation module fails validation or linking, none of its SWIs becomes
visible.

#### 4. Publish initial SWI exports

When the complete foundation set has linked successfully, Rust atomically
publishes the exports declared by their manifests. The first namespace may
include module management, errors, character I/O, task identity, logical
memory, system information, and the minimum command services required for the
next boot phase. Every entry is owned by a foundation module.

#### 5. Start the module system

Foundation-module start definitions now run with the initial SWI environment
available. `ModuleManager` implements public module operations by calling
protected module-runtime primitives. It does not replace the native safety
mechanisms beneath it.

The module lifecycle is:

```text
unloaded -> validated -> linked -> published -> starting -> active
         -> quiescing -> retired
```

#### 6. Load the normal system

The `Boot` module loads the remaining modules from the capsule or, after the
filing system is active, from the system volume. These may include FileSwitch,
HostFS, FontManager, ColourTrans, graphics, Wimp, Filer, BASIC, configuration,
networking, the inspector, and the desktop.

The boot capsule should contain enough to reach a usable recovery surface. It
need not contain every normal desktop component. The current implementation
offers only the restricted native retry/alternate/exit surface; it is not yet a
recoverable BASIC64 command line.

#### 7. Enter the configured environment

The BASIC64 `Boot` module reads startup policy and selects the command
environment, BASIC64, desktop, or a recovery configuration. Rust contains no
policy mapping a configured language or desktop choice to a special host path.

### Native boot responsibilities

| Concern | Hard-coded in Rust? |
|---|---:|
| Public SWI numbers or names | No |
| Public SWI implementations | No |
| SWI-to-module registrations | No; declared by manifests |
| SWI dispatch mechanics | Yes |
| Caller context and checked logical memory | Yes |
| SWI error/X-bit transport mechanics | Yes |
| BASIC64 execution machinery | Yes |
| Module validation, linking, and lifecycle machinery | Yes |
| Capability enforcement | Yes |
| Emergency boot diagnostics | Yes, but not as a SWI |
| Machine-facing and host primitives | Yes, private and capability-protected |

`OS_Module` belongs to `ModuleManager`; initial installation happens directly
from trusted manifests. `OS_WriteC` belongs to `Console`; native emergency
diagnostics are not `OS_WriteC`. Public error services belong to `Error`; the
runtime uses an internal structured-result mechanism before they exist. Public
memory services belong to `Memory`; internal runtime allocation is not a SWI.

### Recovery

If the boot capsule cannot be validated or foundation modules cannot be linked,
Rust presents a deliberately limited recovery surface containing the failed
stage, module and definition, structured error, capsule/runtime ABI versions,
diagnostic log, and options to retry, select another capsule, or exit.

Recovery is not a second operating environment. It provides no normal command
line, user-file access, or SWI environment.

## Desktop projection

The desktop is a user-oriented projection of the same live system, not a
privileged opaque layer above it. Applications remain useful boundaries for
namespaces, permissions, persistence, lifecycle, packaging, and failure
containment, but their implementation need not be sealed.

Interaction should form a continuous progression:

```text
use -> inspect -> configure -> compose -> automate -> program -> modify system
```

Arbitrary runtime entities must not be dumped directly onto the desktop. The
same entity can have progressively deeper projections:

- a friendly desktop representation;
- operational controls;
- diagnostic status;
- ownership and relationship views;
- source, IR, and native-code representations.

The initial proof of the live-system experience should be concrete: navigate
from a visible window to its event task and BASIC64 handler, replace that
handler, and observe the existing window adopt the new behaviour without being
restarted.

## Persistence

Initial persistence covers definitions and reconstructable meaning rather than
arbitrary suspended machine state:

- BASIC64 source and parsed definitions;
- named durable objects and data;
- desktop relationships and arrangements;
- task definitions, not arbitrary native stacks;
- provenance required for inspection;
- explicit recipes for reconnecting external resources.

Persistent state, transient resources, and reconnection recipes are distinct.
A file channel may persist its path, access intent, logical position, and
reconnection policy; a host file descriptor does not survive. A network service
may retain enough information to establish a new connection; an old socket is
not resurrected.

Full image persistence and arbitrary continuation restoration are not initial
requirements.

## Staged delivery

The architectural direction does not require every live-system feature to land
at once. A practical sequence is:

1. Give BASIC64 procedures stable identities and versioned definitions.
2. Introduce the module registry and manifest-owned SWI entry cells.
3. Replace direct Rust SWI handlers with BASIC64 wrappers over protected
   primitives, one module family at a time.
4. Produce and load the trusted foundation boot capsule.
5. Expose modules, tasks, resources, and relationships through a read-only
   live registry.
6. Build the system browser over that registry.
7. Add capability-controlled live replacement and state migration.
8. Add persistent definitions and reconnection recipes.
9. Extend desktop composition only after the underlying identities and
   permissions are proven usable.

The current Rust `SwiDispatcher` and its hard-coded handlers are transitional
scaffolding. Migration should preserve working public behaviour while moving
ownership behind module entry cells; it should not require a single disruptive
rewrite.

## Architectural tests

Future work packages should preserve these invariants:

- No public SWI name, number, or semantic handler is required by native boot.
- Every published SWI identifies an owning module and BASIC64 definition
  generation.
- A module cannot import or invoke a Rust primitive without a granted
  capability.
- SWI pointer arguments remain logical and caller-scoped; modules never receive
  unchecked host pointers.
- Publishing a foundation export set is atomic.
- Active calls can finish against retired definitions safely.
- Failed replacement leaves the previous module generation active.
- Inspection alone grants no operational authority.
- Derived native code can be discarded and rebuilt from retained source and
  dependency information.
- A failed boot remains diagnosable without a functioning SWI environment.

## Remaining design work

The following questions remain open for work-package design:

- the exact spelling and grammar of the agreed BASIC64 System Profile features;
- the portable IR and boot-capsule format;
- the first primitive ABI and capability vocabulary;
- the initial foundation-module split and dependency graph;
- the precise historical SWI contracts retained by each module;
- structured register and memory descriptors for SWI arguments;
- module state migration and compatibility rules;
- scheduling and concurrency semantics for active replacement;
- bounded provenance storage and observation APIs;
- inspector interaction and progressive disclosure;
- persistence schemas and reconnection failure behaviour;
- signing, trust, recovery, and user-authorised system modification.

These should be resolved through concrete vertical slices rather than by
building a universal object model in advance.
