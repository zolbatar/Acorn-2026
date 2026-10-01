# Ricochet implementation phases and work packages

## Purpose

This document turns the decisions in
[`ricochet-architecture.md`](ricochet-architecture.md) into an incremental delivery
plan. Each phase leaves Ricochet runnable and testable. Public SWI behaviour is
preserved while ownership moves from the current Rust `match` dispatcher to
versioned BASIC64 modules.

The phases are dependency ordered. Work packages inside a phase may proceed in
parallel only where their listed dependencies allow it.

## Delivery rules

Every work package must follow these rules:

- preserve documented public SWI contracts unless the package explicitly
  records and justifies a change;
- keep logical addresses caller-scoped and never expose unchecked host pointers;
- retain the interpreter as the reference BASIC64 execution path;
- keep the current system runnable at the end of the package;
- add tests at the new architectural boundary rather than relying only on
  end-to-end snapshots;
- distinguish public SWIs from private capability-protected primitives;
- keep source and manifest data inspectable even when derived code is cached;
- update the design documents when a package resolves an open question;
- remove transitional paths once their final replacement is proven.

### Compiler boundary decision for Phases 0–3

These phases deliver a portable typed IR and one authoritative interpreter,
not native System Profile JIT/AOT code. The current module contract includes
mutable private workspace, task-owned checked logical memory, capability-gated
host primitives, lifecycle transactions, and replaceable definition
generations. Native lowering before those checked call/state interfaces are
stable would duplicate authority and error behavior in a second execution
engine. Accordingly, WP0.4/WP2.1/WP2.4 require the typed IR to represent and
execute the supported 0.1 constructs through a checked interpreter adapter,
and require JIT/AOT requests to be explicit and reject unsupported module
lowering. They do not require a native module target. The IR contains no saved
parser AST payload; adding a compiler later requires lowering the typed IR,
not recovering semantics from source-parser nodes.
Native admission currently treats standalone `MODE=BASIC64` programs as
interpreter-only as well, since their exact integral values must not enter an
f64 compiler unnoticed. CLASSIC/HYBRID numeric literals retain the legacy
floating model and existing native paths.

## First implementation checkpoint — 2026-09-29

This first delivery establishes the Phase 0–3 Console vertical slice, but it
does not claim that every planned exit criterion is complete. The exact package
gaps below are part of the handoff, not silently deferred scope.

| Package | Delivered in this checkpoint | Still open |
|---|---|---|
| WP0.1 | Versioned YAML inventory covers every numeric handler, Wimp entry, recognized named alias, and named-only project service. | Inventory generation/validation from dispatcher declarations is not automated. |
| WP0.2 | Compatibility matrix maps each service family to concrete tests or explicit gaps; Console bounds, ReadLine edge cases, and routes have focused tests. | The matrix is not a full register-by-register golden test suite; many historical reason/flag cases remain partial. |
| WP0.3 | `REM @...` source metadata and deterministic `RICOCHET-MANIFEST\t1` serialization cover module identity/version, source identity, language/target profile, dependencies, symbol imports/exports, primitive imports, requested capabilities, SWI contracts, lifecycle hooks, and replacement policy. Validation rejects malformed schema rows, duplicate fields/exports/imports/hooks, invalid paths/contracts, unresolved link metadata, and imports without requested/granted capabilities. The Console module needs no manifest exception. | The schema is a strict internal tab-separated wire format, not a standard YAML/JSON package format; inventory generation from live dispatcher declarations and cryptographic source/package authenticity remain later work. `MIGRATING`/`RESTART` policies are metadata only; only compatible live replacement is implemented. |
| WP0.4 | The provisional System Profile 0.1 constructs in this slice have executable interpreter semantics and a separate source-located typed IR: records, enums/flags, typed signatures/results, structured errors, opaque handles, read-only module/record/local bindings, imported PROC/FN calls, managed-resource rights, and exact INT64/UINT64 literals, arithmetic, comparisons and loops. The IR preserves module/version/source/dependency identity, definition signatures, type/workspace schema, and checked caller-task address/memory semantics. CLASSIC/HYBRID gates and Console, Error, FileSwitch and Wimp fragments are tested. | No native System Profile JIT/AOT lowering or serialized IR/package ABI exists. Unsupported compiled requests fail at the shared admission boundary; interpreter semantics remain authoritative. Exhaustive historical SWI error/X-bit conformance and authenticated source/package verification remain open. |
| WP1.1 | Opaque IDs exist for modules, instances, definitions, generations, SWI cells, capabilities, and primitives. | IDs are process-local and are not persistent, by design. |
| WP1.2 | Versioned SWI cells retain active old generations; a real threaded replacement test verifies old-call completion and new-call acquisition. The cell retains weak retirement metadata; the active invocation lease owns the strong reference, so the old generation is freed as the last lease exits. Dispatcher source-program mappings are swept after a leased call returns or during replacement, retaining the old parsed source only while an active old generation needs it. | Weak retirement metadata is pruned during inspection/collection or the next replacement. |
| WP1.3 | Validation, linking, atomic publication, start, quiescence, and retirement are separate registry transitions. Failed start removes the module's entire SWI set before returning to `Linked`; retry is possible, and active calls retain their generation. | Hook execution is supplied by the BASIC64 module-management path in WP2.3. State migration remains a future contract; lifecycle failure diagnostics are returned to the manager but not retained as a structured registry record. |
| WP1.4 | Typed primitive descriptors resolve at link time; imports, grants, and active-module invocation are enforced. Console VDU/graphics stream parsing is behind the distinct `Host.Graphics.AcceptByte` mechanism, separate from raw `Host.Console.WriteByte`. | Primitive descriptors are register-shaped rather than a general typed native-call ABI; broader policy/mechanism extraction proceeds with later SWI owners. |
| WP1.5 | Public read-only reflection resolves definitions, exports, types, source locations, and manifests; Rust diagnostics trace an entry cell to owner, definition, source path, and generation. | No user-facing inspector is part of these phases. Reflection intentionally grants no edit or capability authority. |
| WP2.1 | Each source unit has its own namespace, visibility/import/export validation, private persistent workspace schema, type metadata, retained source locations, executable cross-module PROC/FN imports, and a distinct portable typed IR. The IR contains structured operations rather than a saved parser-AST payload and can reconstruct the common reference-interpreter program. Same-named private definitions coexist. | The IR is not a serialized package ABI, and native compilation is unsupported. Type checking and execution share the interpreter's authoritative semantics through an explicit checked IR adapter. |
| WP2.2 | Primitive calls execute in the interpreter and retain active module, caller task, task memory, and capability context. | Register-oriented prototype only; no generalized typed return values or shared IR lowering. |
| WP2.3 | BASIC64 `Start`, `Quiesce`, and `Finalise` hooks run through a trusted Rust-side module manager against private persistent state. Tests prove state survives calls/hooks, is shared through live replacement, is removed on retirement, and a throwing `Start` unpublishes every export. A throwing `Quiesce` restores the prior workspace and reopens admission; a throwing `Finalise` restores workspace state, leaves the module quiesced with exports inaccessible/source retained, and permits repair/retry. Retirement is rejected before `Finalise` unless quiescence succeeded. | State migration between incompatible workspace schemas is rejected, not implemented. Lifecycle workspace transactions cannot undo irreversible host effects from a primitive; hooks must defer them until success. Guest code cannot manage modules or call these hooks as authority-bearing management operations. |
| WP2.4 | Invocation plans distinguish `Interpreter`/`Jit`/`Aot` with module-version, source-path/hash, transitive dependency path/hash/version, profile, target, runtime-ABI, definition, and generation identity. Unsupported module JIT/AOT requests fail explicitly. Replacement invalidates derived targets while retained active generations finish. System Profile IR and JIT admission share one checked lowering boundary. | No compiled System Profile module target is produced or cached. Source fingerprints use non-cryptographic FNV-1a change detection; native lowering and authenticated package inputs remain future work. |
| WP3.1 | Numeric dispatch consults module-owned SWIs first and retains a diagnostic transitional route for remaining handlers. | Existing Rust fallback implementations and hard-coded numeric constants remain migration scaffolding; full public-surface migration is Phase 5. |
| WP3.2 | `Host.Graphics.AcceptByte` contains the hosted VDU stream parser/display policy; `Host.Console.WriteByte` is raw host byte output. `Host.Console.ReadByteStatus` supplies input state. Every call is module/capability gated. | The VDU stream mechanism remains a Rust primitive. The later partial WP5.5 slice separately places public `OS_Plot`/`OS_ReadPoint` policy in Graphics; it does not migrate VDU parsing. |
| WP3.3 | All six Console exports, including `OS_ReadLine`, are interpreted BASIC64 definitions with checked caller memory where applicable. Phase 4 now loads them from the source-derived boot capsule. Focused tests cover editing, accepted ranges, echo-only and R4 substitution, full-buffer bell, Escape, EOF, Control-D, CR/LF, and memory-bound behavior. | Rust legacy handler branches remain transitional fallback code. Exhaustive historical ReadLine option combinations and exact error-block cases are not verified. |
| WP3.4 | The public host-side `Basic64ModuleManager` accepts, rejects, and restores a source replacement without restarting; it preserves the entry cell, invalidates derived targets, and old invocation leases retain active generations. Its authority token is private and absent from guest APIs. | This is a trusted in-process management API, not yet a BASIC64 system browser/editor or user-facing authorization workflow. |

The Phase 0–3 runnable demonstration is in
[`ricochet-milestone-0-3.md`](ricochet-milestone-0-3.md); implementation evidence
and compatibility commands are summarized in
[`ricochet-compatibility-matrix.md`](ricochet-compatibility-matrix.md). Phase 4
has begun and its boot-capsule implementation is documented in
[`ricochet-boot-capsule.md`](ricochet-boot-capsule.md). The initial namespace
starts empty and the complete seven-module source-derived foundation capsule
is linked and published atomically. WP4.3 now has a small executable public
service for each required foundation owner; this does not claim broad RISC OS
API coverage or complete Phase 5 SWI migration.

### Phase 4 implementation checkpoint — 2026-09-29

| Package | Delivered in this checkpoint | Still open |
|---|---|---|
| WP4.1 | Deterministic `TRBOOT01` bytes contain visible source, canonical manifest v1, direct dependency graph, primitive imports, explicit host grants, SWI exports, runtime ABI, and CRC-32. Builder validates duplicate SWI owners, unresolved/version-mismatched imports/dependencies, cycles, grants, ABI and corruption. `include_str!` makes source changes rebuild the executable; `--write-boot-capsule`/`--verify-boot-capsule` provide normal inspect/regenerate commands. | CRC-32 detects corruption but does not authenticate alternate capsules; no signature/key trust exists. No native caches are included (they are optional). The current eight-module boot/command set remains a reviewed fixed host allowlist. |
| WP4.2 | Native bootstrap resets to an empty public table, decodes and links directly from bytes without FileSwitch/SWIs, reaches a linked-but-unpublished set, then publishes all capsule exports in one transaction. A focused test observes the empty table before and after linking. | The loader is synchronous and tailored to this hosted substrate; alternate-capsule trust remains explicit operator selection plus fixed capability policy. |
| WP4.3 | The capsule's initial fourteen public exports are manifest-owned BASIC64 definitions. Console owns its six character SWIs; Error owns standard `OS_GenerateError`; Memory owns standard `OS_ChangeDynamicArea` and `OS_DynamicArea`; ModuleManager owns `OS_Module`, `Ricochet_ModuleInfo`, `Ricochet_ModuleLookup`, and `Ricochet_SwiInfo`; TaskManager owns `Ricochet_TaskInfo`; System provides qualified startup-setting/handoff functions; Boot selects Language 0/3 after atomic publication. Rust contributes only declared capability-gated mechanisms for these migrated calls. A failing foundation Start resets module records, workspaces and all entries before recovery. WP5.1 subsequently adds three manifest-owned System query exports. The current WP5.3 introspection slice adds ModuleManager's `Ricochet_ModuleExport`/`Ricochet_DefinitionSource` and `RicochetCommands` ownership of `OS_CLI`. | The foundation APIs are deliberately minimum viable: TaskManager does not create/schedule tasks. Module loading is post-boot only; the native bootstrap never calls public `OS_Module`. Dynamic areas have explicit task-local bounds and omit callbacks/physical mappings. Other public semantics and transitional fallback migration remain Phase 5 work, not hidden bootstrap SWIs. |
| WP4.4 | Restricted native recovery reports stage, module/definition, structured cause and ABI, and permits embedded retry, explicit alternate-capsule path, or exit. It has no normal CLI, guest FileSwitch access, or public SWI. Tests cover corruption → alternate valid capsule, invalid ABI diagnostics and exit, missing alternate path, and embedded retry; a failed foundation Start also proves the public namespace is reset before recovery. | CRC is not cryptographic authentication. |
| WP4.5 | `Runtime::run` no longer reads `Language` or maps it to a special host path. `Boot.bas64` makes the Language 0 versus 3 decision, and host startup only consumes the typed request. Tests exercise both choices; `--stdio` remains the MOS recovery path. | The `DESKTOP` and other still-Rust-backed command implementations are now reachable only through explicit `BRIDGE` descriptors in the BASIC64 `@COMMAND` registry; command recognition and dispatch remain BASIC64-owned. CONFIGURE/STATUS parsing has moved to BASIC64. Config persistence and display-settings attachment remain host mechanisms. |

WP4.3's required foundation set is implemented with a real, intentionally
bounded service for each owner. The overall Phase 4 result remains a first
bootable slice rather than a complete RISC OS system: the explicit `DESKTOP`
command and remaining non-migrated OS_CLI policy, as well as the larger
transitional public surface, still need migration work. Those limits are
explicit, and no empty module shells are used to imply service ownership. The
delivered split and trust boundary are
recorded in [`ricochet-boot-capsule.md`](ricochet-boot-capsule.md).

### Phase 5 / first WP5.3 command checkpoint — partial, 2026-09-30

This checkpoint adds useful post-boot module services, a bounded error/X
return contract, three System query services, one narrow same-title guest
module replacement class, and BASIC64-owned MOS inspection, configuration,
classic module commands, registry-backed Help, and a bounded system-variable
foundation. Public inspection is now
`*INSPECT`; module lifecycle routes use classic star-command names. It is deliberately not reported as WP5.1 complete:
broader replacement behavior, system compatibility, and exhaustive error
compatibility remain open. WP5.3 also remains partial: broad MOS command
coverage, environment-variable substitution, full Obey/Exec compatibility, and
configuration-consumer/UI work remain open.

| Area | Delivered | Still open |
|---|---|---|
| ModuleManager | BASIC64 owns `OS_Module` reason selection/policy and calls the existing `ModuleManagement`-gated LoadSource mechanism. Reasons 1 Load and 4 Delete operate on bounded UTF-8 `&064` source through checked HostFS/task-memory boundaries. New modules publish atomically and run Start with rollback; Delete runs Quiesce/Finalise and protects foundation and depended-on modules. A same-title reason-1 reload accepts only a compatible-immediate source generation: unchanged manifest identity/dependencies/capabilities/lifecycle metadata, SWI names/numbers/definition names/register contracts, exported PROC/FN signatures, and the full persistent/type schema. It preserves ModuleId, instance and entry-cell IDs, shares workspace, commits every export together, and retains old source/generations for leased calls; it does not rerun lifecycle hooks. Incompatibility or parse/link failure leaves the old module active. Case-only title changes preserve installed spelling. `Ricochet_ModuleLookup`, `Ricochet_SwiInfo`, `Ricochet_ModuleExport`, and `Ricochet_DefinitionSource` expose active manifest/source identity without RISC OS process pointers. | Quiescent, migrating, and restart-required replacement; state migration; OS_Module parameters and `%` instantiations; native `&FFA`/ROM modules; arbitrary host capability imports; and historical reasons other than 1 and 4 remain open or explicitly unsupported. PRM same-title Load is destructive (old instantiations are killed before initialization); Ricochet deliberately stages and rolls back instead. |
| Error and X form | `OS_GenerateError` is a BASIC64-owned structured error service. Numeric bit 17 and named `X` calls return a checked caller-task standard error block in R0 and set V on failure; `XOS_GenerateError` specifically preserves its supplied R0 block and sets V; success clears V. Unknown SWIs use generic code 1; structured service codes are retained. `SYS ... TO vars ; flags` exposes V through BBC BASIC flags syntax. | Normal errors still propagate as the existing hosted `RuntimeError`; there is no RISC OS error vector/handler, no exhaustive register/flag behavior matrix, and no per-service historical error-number namespace. |
| System information | `System.bas64` owns `OS_SWINumberToString` (`&38`), `OS_SWINumberFromString` (`&39`), and `OS_ReadMonotonicTime` (`&42`). Names resolve only through active manifest exports; caller buffers use checked task-scoped logical addresses, the `X` prefix maps bit 17, and monotonic time is a wrapping 32-bit centisecond count. Each service uses a `SystemQueries`-gated Rust mechanism. | Only active manifest-owned SWIs can be converted; transitional Rust-only numeric handlers, `OS_WriteI` aliases, and unknown names/numbers are not enumerated and return structured identity errors. Name matching is exact and case-sensitive (except the leading uppercase `X`). The monotonic epoch is hosted runtime initialization, not the hardware reset epoch. Broader queries and exhaustive PRM register/error compatibility remain open. |
| MOS byte/word services | `Mos.bas64` owns public `OS_Byte` (`&06`) and `OS_Word` (`&07`) reason policy. Reasons 21 (`X=0`), 129 (`Y<128`), and 138 (`X=0`) use checked task input/timing mechanisms; word reasons 1-4 use typed 40-bit clock mechanisms and checked five-byte little-endian caller blocks. Numeric and X forms, name-based calls, BBC `CALL &FFF4`/`&FFF1`, and supported `*FX` all route to the same module-owned endpoints. No Rust numeric fallback remains. | This is the explicitly hosted subset, not full PRM hardware/timer behavior. Other byte/word reasons and selectors reject explicitly; native `OS_Byte 198` and hardware services are not implemented. `*FX 151,78,243` remains the existing hosted ClockSP5 no-op. |
| System variables | `System.bas64` owns PRM-numbered `OS_ReadVarVal` (`&23`) and `OS_SetVarVal` (`&24`) over one runtime-scoped Rust store. BASIC64 `*SET`, `*SHOW`, and `*UNSET` own parsing, wildcard policy, Help metadata, and output. Only caller-checked logical buffers and opaque task-local R3 enumeration contexts cross the service boundary. | This is a bounded STRING-only subset: type 0 immediately expands exact `<name>` references, outer/doubled quotes, and printable `|<`, `|>`, `||`, and `|"` escapes; type 4 stays raw UTF-8. Substituted bytes are not rescanned. Numeric `<number>`, wildcard references, control escapes, other operators, macro/code/numeric types, and general command substitution remain unsupported. `Alias$<command>` is separately supported as static type-0/type-4 data through the same store; see the alias row below. `*OBEY` separately supports BASIC64-owned `%0`–`%9`, `%*n`, and `%%` expansion; task-local read-only `Obey$Dir` shadows stored state while that task has an active source frame. `*Exec` and general CLI expansion remain unsupported. Malformed or missing references fail atomically. Names are visible ASCII, 1–32 bytes; inputs, results, and stored values are at most 256 UTF-8 bytes; the store holds at most 128 entries/32 KiB. Lookup is case-insensitive, preserves initial spelling, and enumerates deterministically; `*` and `#` are supported for read/update/delete, with update requiring exactly one existing match. Selectors use checked NUL termination, narrower than PRM's general ASCII <=32 terminator allowance. Hosted `*SET <name>` with no value creates an empty string, whereas PRM *Set requires a value. A separate `SystemVariableWrite` right is granted only to the trusted interactive MOS Task. Values are runtime-session local; host environment and persistence are excluded. No public OS_GSTrans register contract is claimed. |
| Configuration commands | `RicochetCommands.bas64` owns `*CONFIGURE`/`*CONF.` and `*STATUS`, including parsing, supported values/defaults, diagnostics and output. Its six-key v3 surface is Language, WimpMode/Mode, BASICMode, BASICProfile, BASICTarget, and BASICEngine. WimpMode is the single resolution/palette choice; Auto means host-sized full-colour C16M/Rgb888. It uses a checked 4 KiB caller-task dynamic scratch area and capability-gated typed read/write/replace persistence mechanisms; no command policy branch remains in the Rust CLI adapter. BASICProfile permits a single allowed ASCII name up to 232 bytes, fitting the 256-byte OS_CLI contract. `*STATUS` is public read-only; writes require separate `ConfigurationWrite` authority, including `RICOCHET_DISPLAY APPLY`. The trusted MOS Task receives it explicitly; ordinary/spawned tasks do not inherit it. Rust's typed store remains a defensive schema boundary and performs atomic persistence. Corrupt, unsupported, unreadable, invalid-UTF-8, or >64 KiB stored files now use safe defaults without failing `Boot.Start` or changing the file; BASIC64 Boot and `*STATUS` expose a path-redacted recovery cause. The first authorized successful write or DEFAULTS makes a streaming/readable recovery copy before atomically saving canonical v3 data; failed backup/save leaves the damaged file and recovery state intact. Native capsule failure recovery remains a distinct restricted interface. | General non-Ricochet command migration, richer configuration/UI management, and authority administration remain open. The BASIC64 defaults and Rust defensive schema mirror require drift tests; recovery tests cover corrupt files, explicit repair, unchanged state on denial/failure, and separation from capsule recovery. |
| MOS inspection and classic module commands | `tests/ricochet_classic_module_commands.rs::classic_module_commands_and_inspect_share_live_read_only_module_identity`; `tests/ricochet_mos_introspection.rs::wp51_mos_introspection_matches_read_only_queries_and_tracks_live_generations`; `tests/ricochet_authorization.rs` | `RicochetCommands.bas64` owns OS_CLI and exposes read-only `*INSPECT MODULES`, `MODULE`, `SWI`, `DEFINITION` (with read-only `SOURCE` alias), plus `*Modules`, `*RMLoad`, `*RMRun`, `*RMKill`, and conditional `*RMEnsure`. The inspect routes share the same query SWIs; active metadata is public, source requires `SourceRead`, and mutations require separate `ModuleManagement`, all checked on the original Task. Classic command support is limited to `&064` BASIC64 source: no init strings, `%instantiation`, native `&FFA`, ROM/RMA inventory/lifecycle, or separate RMRun application entry. RMEnsure uses numeric `major.minor[.patch]` and runs a bounded command tail only when absent/older; unsatisfied no-tail requests error. Native-only commands report explicit unsupported diagnostics. Help/execution uses the live command registry described below. Other Rust-backed commands appear only as explicit BRIDGE descriptors; there is no general legacy fallback. |
| Module-owned command registry and `*HELP` | `tests/ricochet_command_help.rs::help_and_command_registry_are_live_module_owned_and_read_only`; `tests/ricochet_cli_aliases.rs`; `tests/ricochet_basic_command_surface.rs`; `tests/ricochet_obey.rs`; `tests/ricochet_exec.rs`; manifest/parser unit coverage | `@COMMAND` metadata is published/removed/replaced atomically with its owning module. The active registry is the single source for command execution and Help; it includes explicit closed-allowlist Rust bridge entries, preserves declaration spelling for display, and keeps PROC handlers private. Registry order is case-folded module title then declaration order. Exact and abbreviated execution takes the first match; `*HELP prefix.` lists all matches. Before registry lookup, BASIC64 checks static `Alias$<command>` String/LiteralString variables; exact aliases can shadow registered commands, unique final-dot prefix aliases resolve, ambiguous prefixes error, and one leading `%` bypasses aliases. Alias substitution supports `%0`–`%9`, `%*n`, `%%`, appends unused arguments, and recursively handles one command per expansion; depth is at most eight, each expansion at most 255 UTF-8 bytes, and total expansion work at most 2,048 bytes. `*SHOW Alias$*` reflects the variable store and `*HELP ALIASES` describes usage, without fabricating registry entries. Type-2 macro variables, full alias command-list semantics, redirection, pipelines, and arbitrary CLI/GSTrans behavior remain unsupported; the source-backed scope and evidence are in `ricochet-cli-aliases-audit.md`. Topics are `Commands`, `FileCommands`, `Modules`, and `Syntax`; module Help reports actual version without invented dates. RISC OS syntax notation is explained. Help currently streams without paging on terminal and windowed surfaces. `BASIC`, `RUN`, and `BASIC64` are the retained public BASIC launch routes; tokenized files run directly through `BASIC`/`RUN`, and configured `BASICEngine` selects Interpreter/Hybrid/Strict. The retired load/cache commands and one-shot engine command are not aliases or registry entries; see `ricochet-command-script-milestone-audit.md` for migration and limits. The initial bounded `*OBEY <guest-path>` subset preserves caller authority and guest path/line provenance; its explicit grammar, newline handling, stop behavior, limits, and unsupported PRM features are recorded above. The distinct bounded `*EXEC [guest-path]` input-source facility is task-local, feeds OS_ReadC/OS_ReadLine and BASIC input, supports atomic replacement, bare-command stop, and EOF fallback, and is audited in `ricochet-exec-audit.md`. General command substitution and automatic script launch remain future work. |

Clarification: the System variables row's `*Exec` wording refers to the absence
of general CLI/GSTrans expansion, not to the separate bounded input-stream
command. The supported task-local `*EXEC` subset is described immediately
above and in the Exec audit.

The current v3 configuration surface has six keys: `Language`, `WimpMode`
(`Mode` alias), `BASICMode`, `BASICProfile`, `BASICTarget`, and `BASICEngine`.
`Language` accepts PRM decimal/`&hex`/`base_num` numeric spellings but only
module IDs 0 and 3. WimpMode takes `Auto` or a bounded
`X<width> Y<height> C/G<depth>` selector for the supported logical size/depth
table; monitor IDs, scaling, and refresh fields are not implemented. WimpMode
alone controls both resolution and palette: Auto follows host content size
and selects full-colour C16M/Rgb888. Old `DisplayResolution`/`DisplayColour`
pairs migrate on file load; fixed pairs become one selector and `Window` maps
to Auto/full-colour. In v2 files explicit WimpMode wins over the retired
`RicochetOutputProfile` row. Old `WindowFurniture` is discarded. These keys
are migration-only, rejected by public commands and full reset. The window
renderer exposes only the approved flat appearance; `WindowFurnitureLayout`
continues to mean geometry.

Evidence is recorded in [`ricochet-boot-capsule.md`](ricochet-boot-capsule.md),
[`ricochet-swi-inventory.yaml`](ricochet-swi-inventory.yaml), and the
[`compatibility matrix`](ricochet-compatibility-matrix.md). The PRM contracts
used for `OS_Module`, the X-form error slice, and the migrated System queries
are linked there. Keep WP5.1 open: broader replacement classes, error and
system compatibility, and the remaining service migration are required; this
slice does not weaken its exit criteria.

## Phase 0 — Baseline and decisions

### Goal

Define the migration boundary precisely and establish behavioural evidence for
the system that will be moved behind modules.

### WP0.1 — Inventory the existing SWI surface

Produce a machine-readable inventory of every currently recognised SWI:

- number and name;
- current Rust handler;
- caller-memory arguments and result registers;
- state or host services used;
- blocking and re-entrancy behaviour;
- current tests and known compatibility gaps;
- proposed owning Ricochet module;
- whether the final BASIC64 definition will be native BASIC64, an alias, or a
  wrapper over a Rust primitive.

The inventory must include named-only project services and Wimp dispatch, not
only the main numeric `match` in `src/swi.rs`.

**Deliverable:** versioned SWI inventory checked into `docs/` in a format that
can later generate or validate registry data.

**Exit criterion:** every current dispatch arm has exactly one proposed owner
and migration class.

### WP0.2 — Capture compatibility tests

Add or identify focused tests for the currently supported contracts, including:

- console and VDU output;
- input and line reading;
- MOS CLI and configuration;
- FileSwitch operations and checked guest buffers;
- graphics and ColourTrans;
- Wimp task/window/event calls;
- named project services;
- X-bit and error behaviour where currently supported.

**Deliverable:** a test matrix linking inventory entries to automated tests or
an explicit compatibility gap.

**Exit criterion:** moving a handler behind a module can be checked without
depending solely on visual or whole-application tests.

### WP0.3 — Freeze the first module and primitive models

Resolve only the syntax and schema needed for the first vertical slice:

- module identity and semantic version;
- definition identity and generation;
- SWI name/number/export declaration;
- primitive import declaration;
- capability request and grant;
- module dependencies;
- load, start, quiesce, upgrade, and finalise entry points;
- public contract metadata for register and logical-memory arguments.

Prefer a small manifest schema paired with ordinary `.bas64` source before
adding extensive new language syntax.

**Deliverable:** manifest schema, worked examples, and validation rules.

**Exit criterion:** the proposed Console module can be described without an
implementation-specific exception.

### WP0.4 — Design BASIC64 System Profile 0.1

Complete the preparatory language work specified in
[`basic64-system-profile.md`](basic64-system-profile.md):

- profile-gated modules, namespaces, imports, exports, and visibility;
- named records, enumerations, and flags;
- typed procedure parameters and function results;
- structured errors;
- managed opaque handles;
- read-only bindings and fields;
- declarative SWI and primitive metadata;
- a strict distinction among managed values, handles, logical addresses, and
  private native values;
- AST, shared-IR, interpreter, reflection, and JIT-boundary representations.

Begin with representative Console, Error, FileSwitch, and Wimp fragments. Use
them to settle the minimum semantics and provisional grammar. Do not add classes,
inheritance, generics, macros, universal message dispatch, or async syntax
without a demonstrated requirement from the first module slice.

**Deliverable:** a versioned System Profile 0.1 language specification,
representative source examples, parser/type-system implementation plan, and
compatibility tests proving classic profiles do not change meaning.

**Exit criterion:** the Console module and its primitive boundary can be written
clearly without strings, magic numeric handles, parallel arrays, or unchecked
address conversions, and the required constructs have defined AST, interpreter,
and IR semantics.

## Phase 1 — Versioned runtime foundations

### Goal

Introduce identities, generations, registries, and primitive capabilities
without changing existing SWI behaviour.

### WP1.1 — Stable identities

Add opaque runtime identities for:

- modules and module instances;
- definitions and definition generations;
- SWI entry cells;
- capabilities and primitive imports.

Identities must not encode Rust pointers. Diagnostic formatting and stable
comparison are required; persistence is not yet required.

**Exit criterion:** identities survive moves of their backing Rust values and
cannot be forged through a guest logical address.

### WP1.2 — Versioned definition cells

Implement entry cells that point to a current definition generation while
retaining retired generations until no active invocation requires them.

Include:

- guarded acquisition of a generation;
- active-call accounting;
- atomic generation replacement;
- retirement and reclamation;
- deterministic rejection of incompatible replacements.

**Exit criterion:** a concurrency test replaces a definition while an older
invocation finishes safely and subsequent invocations enter the new generation.

### WP1.3 — Module registry and lifecycle state machine

Implement the native mechanism for:

```text
unloaded -> validated -> linked -> published -> starting -> active
         -> quiescing -> retired
```

Loading, publishing, and starting must remain distinct. Publishing a set of
foundation exports must be atomic.

**Exit criterion:** failed validation, linking, or start cannot leave a partial
SWI export set visible.

### WP1.4 — Primitive registry and capability enforcement

Create a typed, private primitive registry. Each primitive declares:

- stable internal identifier;
- calling shape;
- required capability;
- logical-memory access rules;
- blocking/re-entrancy properties;
- failure contract.

Primitive access must be resolved during module linking. Arbitrary BASIC64 code
cannot discover and invoke primitives by guessing a number or string.

**Exit criterion:** authorised module code can invoke a test primitive; the
same import is rejected without the required capability.

### WP1.5 — Read-only runtime introspection API

Expose the new module, definition, generation, and entry-cell metadata to Rust
tests and diagnostics. This is an internal API, not yet the user-facing system
browser.

**Exit criterion:** tests can trace a registered SWI from its entry cell to its
owning module and current generation.

## Phase 2 — Executable BASIC64 modules

### Goal

Load a BASIC64 module, resolve declared primitive imports, and invoke a named
definition through a versioned entry cell.

### WP2.1 — Module compilation unit

Implement the agreed System Profile 0.1 constructs required by the first module,
then teach the BASIC64 frontend/runtime to load a module source unit with:

- private module state;
- exported and private definitions;
- retained source locations;
- definition identities;
- module-level dependency metadata.

Do not require JIT support. The interpreter is the first complete execution
path. Parser-only support is not sufficient: every accepted construct must have
defined name resolution, type checking, interpreter behaviour, source metadata,
and an explicit shared-IR/JIT boundary.

**Exit criterion:** two loaded modules can contain the same private procedure
name without collision, while declared exports remain callable.

### WP2.2 — Typed primitive calls from BASIC64

Add the minimum BASIC64 representation needed to invoke a linked primitive.
Calls must preserve module identity, caller task, logical address-space identity,
and capabilities.

**Exit criterion:** a BASIC64 definition can wrap a test primitive without
receiving a raw host pointer or unrestricted dispatcher access.

### WP2.3 — Module state and lifecycle definitions

Support private module workspace and the initial lifecycle definitions:

- load/link validation without public SWIs;
- `Start` after publication;
- `Quiesce` and `Finalise`;
- a placeholder contract for later state migration.

**Exit criterion:** a stateful test module starts, serves calls, quiesces, and is
retired without leaking its workspace.

### WP2.4 — Derived-code boundary

Ensure interpreted, JIT, and future AOT definitions share one invocation and
versioning interface. Record source and dependency hashes even if only the
interpreter is initially active.

**Exit criterion:** backend invocation identity is explicit; replacement
invalidates every derived-target slot for the prior definition/dependency
identity without changing its public entry-cell identity; unsupported module
JIT/AOT requests fail before executing compiled code.

## Phase 3 — First module-owned SWI vertical slice

### Goal

Prove the complete public path using a small Console module while the rest of
the system continues through transitional handlers.

### WP3.1 — Registry-backed SWI dispatch

Change numeric and named dispatch to consult the module registry first. During
migration, an explicit transitional adapter may forward unported calls to the
existing Rust handlers.

The adapter must be visible in diagnostics and must not be representable as a
final compliant module.

**Exit criterion:** registry-owned and transitional SWIs coexist without
changing caller-context or error semantics.

### WP3.2 — Console primitive set

Extract the minimum protected primitives needed by the Console module, such as:

- write one output byte;
- flush output;
- read or await one input byte;
- query input availability.

Keep VDU policy separate from raw host console I/O so later graphics ownership
does not become trapped in the primitive boundary.

**Exit criterion:** primitives contain host mechanisms, not the public semantics
of `OS_WriteC`, `OS_NewLine`, or string traversal.

### WP3.3 — Console BASIC64 module

Implement an initial module-owned subset:

- `OS_WriteC`;
- `OS_WriteS`;
- `OS_Write0`;
- `OS_NewLine`;
- `OS_ReadC`;
- `OS_ReadLine` where practical in this slice.

String and buffer operations must use checked caller-scoped memory views.

**Exit criterion:** inspection traces each migrated SWI to Console BASIC64
source and then, where required, to a protected primitive. Existing compatibility
tests continue to pass.

### WP3.4 — Demonstrate live definition replacement

Replace a harmless Console behaviour through a new definition generation while
the system remains running, then restore it.

**Exit criterion:** new calls use the replacement, an active old call can finish,
and failed replacement leaves the previous generation active.

## Phase 4 — Boot capsule and zero-SWI bootstrap

### Goal

Boot from trusted BASIC64 foundation modules with no hard-coded public SWIs.

### WP4.1 — Boot-capsule format and builder

Define a deterministic, versioned capsule containing:

- BASIC64 source or portable IR;
- module manifests and dependency graph;
- primitive imports and capability grants;
- SWI export declarations;
- runtime ABI and integrity metadata;
- optional discardable native caches.

The builder must reject duplicate SWI ownership, unresolved imports, dependency
cycles that violate lifecycle rules, and incompatible runtime ABI versions.

**Exit criterion:** identical inputs produce identical capsule content and all
foundation source remains inspectable.

### WP4.2 — Native capsule loader

Load and validate the capsule directly from embedded or explicitly selected
bytes. The loader must not use FileSwitch or any public SWI.

**Exit criterion:** tests begin with an empty SWI table and reach a linked but
unpublished foundation set.

### WP4.3 — Foundation module set

Implement the smallest viable set, initially:

- `System`;
- `ModuleManager`;
- `Error`;
- `TaskManager`;
- `Memory`;
- `Console`;
- `Boot`.

The exact split may be adjusted by WP0.3, but no foundation SWI may be installed
directly by Rust.

For the initial hosted cut, `Error` owns `OS_GenerateError`; `Memory` owns
`OS_ChangeDynamicArea` and `OS_DynamicArea`; `ModuleManager` owns
`OS_Module`, read-only `Ricochet_ModuleInfo`, and safe `Ricochet_ModuleLookup` /
`Ricochet_SwiInfo` project queries; and `TaskManager` owns read-only
`Ricochet_TaskInfo`. `OS_Module` is post-boot only and supports PRM reasons 1
Load and 4 Delete for capability-restricted BASIC64 `&064` source. All other
historical reasons are explicit structured rejections, especially reason 18's
pointer-bearing result. A bounded standard X-form error-block/V convention is
implemented, but not the OS error vector or exhaustive SWI-specific mapping.
These narrow services do not imply the broader RISC OS module, task, error, or
memory APIs are migrated. Guest live replacement,
task creation/scheduling, and full error behavior remain explicit later work.
The project extensions are versioned and inventoried as non-historical SWIs.
`OS_DynamicArea` retains its standard number/reason shape where feasible but
uses checked task-local logical areas with documented hosted bounds and rejects
callback/physical-page features.

**Exit criterion:** manifests publish the initial namespace atomically and
module start definitions run only after publication.

### WP4.4 — Native recovery surface

Add a deliberately restricted recovery path for capsule validation, linking, or
foundation-start failure. It displays the failed stage, module/definition,
structured error, ABI versions, and diagnostic log, and permits retry, alternate
capsule selection, or exit.

It provides no ordinary command line, user-file access, or public SWIs.

**Exit criterion:** corrupt and ABI-incompatible capsules fail diagnostically
without entering a partially initialised Ricochet environment.

### WP4.5 — Remove native startup policy

Move command-line, configured-language, and desktop selection into the BASIC64
`Boot` module. Rust should no longer special-case a desktop command or configured
language value.

**Exit criterion:** both command and desktop startup are selected by Boot-module
policy over module-owned services.

## Phase 5 — Migrate the complete SWI surface

### Goal

Eliminate the transitional Rust SWI handler path. Each sub-package ends with
module ownership, BASIC64 source, protected primitives, and preserved tests.

### WP5.1 — Error, system information, and module services

Migrate public errors, system queries, module enumeration/loading/replacement,
and related commands. Ensure unknown SWIs and failed handlers preserve the
chosen RISC OS-compatible error contract.

**Checkpoint:** a bounded `OS_Module` Load/Delete subset, safe manifest
identity/source queries, X-form error-block transport, and three bounded
System queries (`OS_ReadMonotonicTime`, `OS_SWINumberToString`,
`OS_SWINumberFromString`) are implemented, along with basic module/SWI/source
and module-operation star commands. Broader replacement, System query, and
error compatibility remain required for WP5.1 completion.

### WP5.2 — Memory and task services

Migrate logical-memory, dynamic-area/shared-region, task identity, scheduling,
event, and IPC services. Keep allocation, mapping, validation, and scheduling
mechanisms in Rust.

### WP5.3 — MOS CLI and configuration

Move command interpretation, configuration policy, startup language selection,
and command registration to BASIC64 modules. Rust retains only storage and host
mechanisms exposed through capabilities.

Provide MOS star-command access to every introspection operation exposed by
the Phase 6 system browser. Commands and UI must consume the same
permission-checked query API and report the same logical identities,
relationships, and definition generations. Support readable summaries and a
structured form usable by BASIC64 programs. Inspection is read-only by default;
modification commands require separate, explicit authority.

**Command-runtime checkpoint:** BASIC64 now publishes command identity,
category, syntax, description, order, and handler through each active module's
`@COMMAND` manifest rows. `RicochetCommands.bas64` recognizes commands and
renders `*HELP` from the same live registry; it does not maintain a parallel
name/help table. Help supports the RISC OS topics `Commands`, `FileCommands`,
`Modules`, and `Syntax`, and exact command/module topics. Help abbreviation
queries list every matching descriptor; execution uses the first exact or
final-dot-prefix match in hosted order (case-folded module title, then source
declaration order). Registry publication/replacement/unload follows module
lifecycle transactions. Transitional Rust implementations are explicit,
closed-allowlist `BRIDGE` descriptors in the trusted command module; unknown
commands do not fall through to Rust. Command-only modules are supported, and
their handler procedures stay owner-private rather than becoming symbol exports.
Help streams output without a page-wait prompt on both terminal and windowed
surfaces because the current console does not provide a safe surface-specific
pager. No module dates or host addresses are fabricated.

The follow-on command roadmap is: finish Help and registry identity first;
the first bounded guest-variable substrate is now in place; the initial
`*OBEY <guest-path>` source-execution slice is implemented through the same
registry and invocation context. Rust provides checked guest file reads and
bounded task-local source frames; BASIC64 classifies and dispatches each line.
The subset accepts LF/CRLF/CR, blank lines, leading-space `|` comments, and
EOF without a final newline; it stops on the first error with guest path/line
provenance, retains earlier command effects, and unwinds on `QUIT`. Bounds are
65,536 active source bytes, 255 bytes per line, 4,096 physical lines per
session, and eight nested sources. BASIC64 expands `%0`–`%9`, `%*n`, and `%%`
once per line before dispatch: parameters split on spaces outside double
quotes, quote spelling is preserved, absent parameters become empty, and `%*n`
preserves the raw suffix from the nth argument. Unmatched quotes and incomplete
`%*` forms fail before the affected line; expanded lines over 255 UTF-8 bytes
are rejected without truncation. A task-local, read-only `Obey$Dir` is exposed
through OS_ReadVarVal and type-0 expansion while a frame is active. It contains
the resolved guest parent path and shadows a stored value only for that frame;
nested success, error, and QUIT naturally restore the previous frame/value.
This stable guest-path choice differs from PRM's parent fragment of the path
as invoked. PRM `-v`/`-c`, general command substitution, and
automatic `!Boot` execution remain deferred. `*EXEC` is a separate bounded
task input-source facility described below and in the Exec audit. This is a useful hosted subset,
not a claim that arbitrary RISC OS command scripts run unchanged.

The separate hosted `*EXEC [guest-path]` input-stream slice is now implemented
and is not an extension of Obey's line-dispatch engine. One task-scoped checked
guest file feeds the shared `OS_ReadC`/`OS_ReadLine` path and BASIC input before
queued/host bytes. Replacement is atomic, bare `*EXEC` stops it, and EOF
returns to normal input. It accepts preflighted UTF-8 text with tab and CR/LF,
normalizes LF/CRLF to CR, and delivers a final unterminated line. Limits are
65,536 bytes, 255 bytes per line, and 4,096 lines; native `OS_Byte 198` controls
and full input-source compatibility remain out of scope. See
[`ricochet-exec-audit.md`](ricochet-exec-audit.md).

Before execution, the bounded source is preflighted as UTF-8: embedded NUL and
all control characters except tab and CR/LF are rejected at their guest
physical line, so no shortened command can be dispatched. Once accepted,
shared BASIC64 CLI normalization removes leading stars and spaces/tabs before
recognizing `|` comment lines; internal pipes remain part of command data.

**Bounded command/script milestone — command-surface cleanup delivered,
2026-09-30:** the redundant load/cache and one-shot JIT CLI routes were removed.
`BASIC` and `RUN` each accept a guest file path and run source or tokenized
programs immediately; `BASIC64` retains its native mode/text launch options.
`BASICEngine` is the durable Interpreter/Hybrid/Strict selector for subsequent
file and desktop runs. The per-task loaded-tokenized-program cache and the
one-shot engine-override plumbing are removed; tokenized files are no longer
retained between commands. Strict benchmark-validation remains internal to
acceptance tooling, not a public CLI option. The Hybrid/Strict backends remain
implemented behind `experimental-jit`. This is a bounded command/script
milestone, not WP5.3 completion or completion of Phase 5; general GSTrans,
native Macro variables, broader Run$Path, redirection/pipelines, `OS_Byte 198`,
and the other roadmap gaps remain deferred. See
[`ricochet-command-script-milestone-audit.md`](ricochet-command-script-milestone-audit.md).

### WP5.4 — FileSwitch and filing systems

Implement public file semantics, path policy, file handles, and FileSwitch
routing in BASIC64. Keep host filesystem access and checked bulk I/O as protected
primitives.

Checkpoint (2026-10-01, partial): `modules/FileSwitch.bas64` owns the bounded
`OS_File` (`&08`) reasons 0–12, 16–18, and 255; `OS_Find` (`&0D`),
`OS_BGet` (`&0A`), `OS_BPut` (`&0B`), `OS_Args` (`&09`), and `OS_GBPB`
(`&0C`) reasons 1–10 over checked task-owned channel, transfer, object, and
catalogue mechanisms. BASIC64 selects OS_File path-source policy and bounded
candidate order: reasons 5/255 use runtime File$Path, 12/13 use the R4 path
string, 14/15 use the R4 path-variable name, and 16/17 bypass search. Lists
are strict UTF-8, at most 255 bytes and 16 candidates; candidates are capped at
4096 bytes and resolved only through the caller's guest volume. Only String /
LiteralString path variables are accepted. Wildcards, Run$Path, arbitrary
macro/GSTrans expansion, timestamp semantics, and global cross-Task open-file
tracking remain unsupported. Loads/saves are capped at 1 MiB, and complete
caller-memory spans are checked before effects. Reasons 7/11
retain hosted truncation for an existing unlocked file. It covers direct
guest-path channel opens in three
modes, close-one/close-all, byte I/O, Args reasons 0–5 and 7, atomic
canonical-name output, file block transfer with full caller-span preflight,
and bounded directory snapshots/records. GBPB transfer and staged catalogue
output are each capped at 1 MiB; a snapshot is capped at 4096 raw entries.
Successful zero-count reasons 1–4 validate the caller span, channel access,
and file range before applying PRM positioning semantics: reason 1 seeks to
its explicit offset and may zero-extend the file; reason 3 seeks when its
offset is at or before extent, but leaves the sequential pointer unchanged
when beyond extent; reasons 2/4 leave the sequential pointer unchanged. All
clear the EOF-error-on-next-read flag and return zero-count register/carry
results.
Checkpoint (2026-10-01, additional partial delivery): `FileSwitch.bas64` now
owns `OS_FSControl` (`&29`) reasons 0, 1, 5–9, 11, 13, 14, 18, 19, 22, 25,
31, 33, 37, 39, 40, 43–45, and 50. BASIC64 selects reason and register
policy; checked Rust mechanisms provide bounded catalogue snapshots, path
bytes, task-local directory/filing-system state, and the shared HostFS volume
label. Reason 11 returns the recognized HostFS selector and prior selector,
with special fields unsupported and rejected atomically. Reason 37 supports
bounded R3 path-variable/R4 path-list sources, ordered guest candidate lookup,
qualified-path bypass, and final-attempt canonicalization without general
GSTrans/macros. Reasons 7/8 use a consistent library-relative path for title
and entries. The filesystem control-block result is a guest logical identity,
not a host pointer. The `LIB`, `EX`, and `INFO` CLI commands are not registered.

This does not complete WP5.4: GBPB reasons 11–12, Run$Path,
wildcard/macro search, exact native error blocks, and BASIC file
statements are not migrated here. Host I/O errors can leave partial external
file effects. See
[`ricochet-fileswitch-channel-audit.md`](ricochet-fileswitch-channel-audit.md)
and [`ricochet-gbpb-ownership-audit.md`](ricochet-gbpb-ownership-audit.md).
The bounded FSControl subset and its deviations are recorded in
[`ricochet-fscontrol-ownership-audit.md`](ricochet-fscontrol-ownership-audit.md).

Preserve caller provenance for retained buffers and asynchronous operations.

### WP5.5 — VDU, graphics, fonts, and ColourTrans

Move public VDU parsing and graphics/colour/font policy into modules where
reasonable. Retain bounded raster access, host rendering, font engine calls, and
GPU submission as Rust primitives.

The partial ownership slice now includes `Graphics.bas64` for `OS_Plot` (`&45`)
and `OS_ReadPoint` (`&32`) register/result policy. Rust mechanisms resolve the
caller window or bounded task-default raster. The VDU stream parser, raster
algorithms, other graphics/colour/font SWIs, and renderer remain outside this
slice. `ColourTrans.bas64` now owns the three existing name-only hosted
`ColourTrans_*` calls, with no numeric identities: HSV conversion is BASIC64
policy, SetGCOL uses the protected caller-raster mechanism, and WritePalette is
an explicit accepted no-op. Native numeric ColourTrans and palette mutation
remain deferred; this does not complete WP5.5 or Phase 5.

The work package must explicitly document any hot path retained as a primitive
and why it is mechanism rather than public policy.

### WP5.6 — Wimp and desktop service boundary

The first bounded lifecycle slice is delivered: `Wimp.bas64` owns
`Wimp_Initialise`, `Wimp_CloseDown`, and `Wimp_StartTask` over checked
caller-memory, registration/cleanup, and launch-queue mechanisms. It has no
numeric Rust fallback when the owner is inactive. Hosted deviations are
recorded in the design brief and lifecycle audit: null R3 is accepted for
versions 300/310, message lists are preflighted but do not filter messages,
CloseDown requires the exact caller handle, and StartTask supports only
`*Commands`, `*BASIC`, and `*BASIC <guest-path>`. This does not migrate the
remaining Wimp SWIs or complete WP5.6; continue the remaining public dispatch,
poll, window, and input policy work without treating Phase 5 as complete.

The next bounded window-state slice is delivered in the same module:
`Wimp_OpenWindow`, `Wimp_CloseWindow`, `Wimp_GetWindowState`, and
`Wimp_SetExtent` are BASIC64-owned block decoders/results over narrow,
caller-owned atomic geometry/stack operations. Their public numeric Rust
fallbacks are removed. Open supports -1, -2, and a positive live sibling
handle; -3/backwindow remains explicitly unsupported. GetWindowState returns
all nine words of its 36-byte block; SetExtent preserves the hosted R0=0
result. This does not complete WP5.6: creation, icons, polling, redraw/update,
menus, pointer queries, and the rest of Wimp policy remain outstanding.

### WP5.7 — Project-specific services

The existing `RICOCHET_DESKTOP` and `RICOCHET_DISPLAY` named-only services are
now routed through the closed module-owned definitions
`DesktopServices.bas64::DESKTOPSERVICE` and
`DisplayManager.bas64::DISPLAYSERVICE`. Their current action/version/enum and
register policy is inspectable BASIC64; bounded HostFS, Wimp menu and display
apply/query mechanisms remain Rust. No numeric IDs were invented. Display
apply still checks the original caller's `ConfigurationWrite` right. This is a
bounded WP5.7 slice, not completion of the package: audit and migrate remaining
project services individually, and do not treat WP5.7 as complete.

### WP5.8 — Delete transitional dispatch

Remove numeric and named Rust semantic matches, direct handler ownership, and
the transitional adapter. Generate or validate name/number lookup solely from
active module manifests.

**Phase exit criteria:**

- every public SWI reports an owning module and BASIC64 definition generation;
- no Rust source maps a public SWI number or name directly to semantics;
- disabling a required module removes its SWIs cleanly;
- compatibility and end-to-end tests pass through registry dispatch;
- the system boots from its capsule into both command and desktop environments.

## Phase 6 — Live system browser

### Goal

Make the new architecture understandable before making broad modification
available.

### WP6.1 — Read-only live graph

Expose bounded relationships among modules, definitions, SWIs, tasks, active
calls, logical memory, files, windows, and resources. Separate identity,
metadata visibility, and observation permissions.

The query API is shared by MOS star commands and the graphical browser. Every
new UI introspection operation must have an equivalent star-command route,
including relationship traversal and retained-source inspection; permissions
must be enforced by the shared service rather than by either presentation.

**Partial implementation checkpoint:** `RicochetCommands.bas64` owns the
`OS_CLI` entry, `*INSPECT` read-only query presentation, the supported classic
module star-command subset, and `*CONFIGURE`/`*STATUS` configuration policy.
The inspect routes use the shared read-only query API. `*Modules`, `*RMLoad`,
`*RMRun`, `*RMKill`, and conditional `*RMEnsure` are implemented; other known
ROM/RMA-dependent classic commands report explicit unsupported errors.
`*RICOCHET` is no longer recognized or advertised. Rust supplies checked
persistence and a capability-gated legacy-command bridge; other MOS command
policy has not migrated. Configuration writes require the caller Task's distinct
`ConfigurationWrite` right, deliberately granted to the interactive MOS Task;
status is explicitly public read-only. Active module/SWI identity is an
explicit public metadata class; retained source is guarded by task-scoped
`SourceRead`, and `OS_Module` mutation by separate `ModuleManagement` rights.
The host bootstraps the interactive MOS task explicitly; ordinary spawned
tasks do not inherit either right, and a privileged provider does not confer
its grant on the original requestor. Code running within the trusted session
shares its task principal, so sandboxing arbitrary programs requires an
ordinary separate task. Coverage is limited to modules, SWI identity, and
retained active definition source; stable opaque IDs for all entities,
relationship expansion, task/resource/window families, and the graphical
browser remain follow-up work.

### WP6.2 — Provenance and statistics

Track useful bounded provenance:

- owner and creator;
- current users and active calls;
- definition dependencies and callers;
- execution counts and failures;
- resource reconnection description where available.

Do not retain an unbounded history of all events.

### WP6.3 — System browser UI

Build a BASIC64 browser that can navigate:

```text
visible entity -> owner/task -> service -> definition -> source/IR/native form
```

Use progressive disclosure; ordinary desktop use must not resemble a debugger.

**Phase exit criterion:** from a visible window, a user can navigate to its task,
event handler, owning module, and retained BASIC64 source without receiving
modification authority.

The same inspection and traversal must be possible through MOS star commands.
Parity tests must verify equivalent identities, generations, relationships,
and access-denied results through both presentations.

## Phase 7 — Safe live modification

### Goal

Turn read-only understanding into controlled live change.

### WP7.1 — Replacement validation

Check public contracts, dependencies, capabilities, retained-reference rules,
and runtime ABI before accepting a definition or module generation.

### WP7.2 — Quiescence and state migration

Implement immediate, quiescent, migrating, and restart-required replacement
classes. Add checked state-upgrade definitions with rollback on failure.

### WP7.3 — Deoptimisation and cache invalidation

Invalidate guarded calls, JIT code, and derived caches when a dependency changes.
Active frames continue under their retained generation where safe.

### WP7.4 — Authorised editing workflow

Add explicit user-authorised modification capabilities, source editing,
validation diagnostics, accept/reject, rollback, and generation history.

**Phase exit criterion:** navigate from a visible window to its BASIC64 event
handler, replace the handler, and observe the existing window adopt the new
behaviour without restart; a rejected edit leaves the running system unchanged.

## Phase 8 — Persistence and desktop composition

### Goal

Persist live meaning and expose selected live entities through approachable
desktop projections.

### WP8.1 — Persistent definitions and durable identities

Persist module source, manifests, selected definition generations, durable data,
and required provenance. Define identity reconstruction and conflict rules.

### WP8.2 — Resource reconnection recipes

Separate persistent state, transient resources, and reconnection recipes for
files, networks, devices, windows, and tasks. Never serialise host descriptors or
pretend an external connection survived.

### WP8.3 — Desktop projections

Allow selected projects, tasks, services, and resources to have user-oriented
desktop representations with progressively deeper inspection. Projection does
not confer authority over the underlying entity.

### WP8.4 — Recovery and compatibility across upgrades

Define persistence-schema migration, missing-resource behaviour, rejected
reconnections, safe-mode loading, and recovery from incompatible module
generations.

**Phase exit criterion:** a selected live project can persist its definitions,
durable state, relationships, and reconnection intentions across a restart
without attempting to restore invalid native stacks or host handles.

## Recommended first delivery milestone

The first implementation milestone should consist of Phases 0–3 only. It proves
the architecture before boot and wholesale SWI migration amplify its mistakes.

The milestone demonstration is:

1. Start the existing Ricochet environment normally.
2. Inspect `OS_WriteC` and see its Console module, BASIC64 source, generation,
   capability grant, and host-output primitive.
3. Invoke it through ordinary `SYS` and existing BASIC output paths.
4. Install a compatible replacement definition.
5. Observe subsequent output use the new generation.
6. Reject an invalid replacement without disturbing the active generation.
7. Restore the original generation.

This vertical slice establishes module ownership, protected primitives,
versioned dispatch, introspection, and safe replacement while touching the
smallest practical service family.

## Dependency summary

```text
Phase 0  baseline and decisions
         including BASIC64 System Profile 0.1 design
   -> Phase 1  identities, registries, capabilities
      -> Phase 2  executable BASIC64 modules
         -> Phase 3  first module-owned SWIs
            -> Phase 4  boot capsule and zero-SWI bootstrap
               -> Phase 5  complete SWI migration
                  -> Phase 6  system browser
                     -> Phase 7  safe live modification
                        -> Phase 8  persistence and composition
```

Read-only introspection foundations begin in Phase 1, but the user-facing
browser waits until module ownership is complete enough to present a coherent
system. Persistence waits until identity, replacement, authority, and migration
semantics have been exercised in the live system.
