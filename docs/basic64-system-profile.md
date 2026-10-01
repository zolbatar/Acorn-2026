# BASIC64 System Profile

## Status

This document records the agreed language direction and the provisional,
executable grammar for the native BASIC64 profile used to implement Ricochet
modules and system policy.

The feature categories and semantic boundaries are firm. The spelling below has
now been exercised by Console, Error/FileSwitch, Wimp, System-query, and MOS
command definitions, but remains System Profile 0.1 syntax under review rather
than a frozen long-term language ABI.

This profile is an additive native language. It does not silently change BBC
BASIC V/VI compatibility behaviour.

## Executable subset checkpoint — 2026-09-29

The first vertical slice now exercises more than parser-only syntax:

- `REM @SYSTEM_PROFILE 0.1` modules declare versioned dependencies, symbol and
  primitive imports, requested capabilities, SWI contracts, lifecycle hooks,
  replacement policy, private state, and public/private symbols. A deterministic
  `RICOCHET-MANIFEST\t1` wire form round-trips the resolved manifest. The
  decoder also accepts the previous pre-release `TRELLIS-MANIFEST\t1` header;
  both forms reject missing, duplicate, unknown, or invalid fields.
- `RECORD`, `ENUM`, `FLAGS`, `ERROR`, and `HANDLE` declarations are resolved
  against typed signatures, state, fields, primitive results, and SWI register
  contracts. Records and errors are managed values; handles remain nominal and
  cannot be numerically coerced.
- Procedures and functions receive runtime checked typed parameters; every
  function has an explicit typed result. `THROWS`, `THROW`, and `TRY`/`CATCH`/
  `ENDTRY` have interpreter behavior, including procedure/loop unwinding and
  structured error values.
- `LET READONLY name AS Type = expression` declares typed bindings scoped to
  one PROC or FN call. Initializers can read typed parameters; an assignment to
  the binding is rejected, and nested calls restore any shadowed value.
- Decimal integral literals and typed `INT64`/`UINT64` values keep full-width
  integer representations through checked arithmetic, comparisons, bitwise
  operations, `ABS`/`INT`/`STR$`, and integer `FOR` loops. Mixing a typed 64-bit
  value with floating arithmetic, or converting one to floating point when it
  is not exactly representable, fails instead of silently rounding.
- `@STATE` is stored in a private per-module workspace that persists across
  calls and lifecycle hooks. `READONLY` state allows one initial assignment,
  then becomes immutable; record fields can be individually read-only.
- `PRIMITIVE` calls are link-validated for imports, capabilities, and register
  shape. The interpreter supplies the reference implementation and keeps
  caller-scoped logical memory; BASIC never receives a host pointer.
- `@IMPORT_SYMBOL` and `@EXPORT`/`@PRIVATE` are resolved and visibility-checked
  during linking. `PROC Module.Symbol` and `FN Module.Symbol(...)` execute only
  when the caller has a linked import and the active provider exports that
  symbol. Provider workspace changes commit on success and roll back on failure.
- Opaque handles are managed identities, not pointers. Primitive descriptors
  declare a required right for each handle argument; the active call checks
  handle type, owner task, registration, and right before the resource is used.
- Each module definition retains its source path, definition identity, profile,
  target and deterministic source fingerprint. FNV-1a is change-detection data,
  not a cryptographic integrity or authenticity check.
- Public read-only `SystemModule::reflection()` exposes the resolved manifest,
  type declarations, definition/export ownership, source lines, typed parameters
  and results, declared error types, and private-state schema. It grants no edit,
  invocation, or capability authority.

This is the executable System Profile 0.1 subset for the Phase 0–3 module
milestone, not a frozen long-term language ABI. The source-located high-level
IR in `src/basic_compat/system_ir.rs` represents expressions, places,
traditional BASIC statements, typed primitives, calls, and control flow using
portable IR data; it stores no parser-AST payload. It also carries
module/version/source/dependency identity, definition signatures, type and
workspace schemas, and checked caller-task `ADDRESS32` arithmetic and logical
memory access. `SystemModule::invoke` prepares the IR and reconstructs the
common reference-interpreter program from those IR operations. Hybrid, Strict,
and AOT requests cross the same explicit admission boundary and reject native
System Profile lowering with a source location; Hybrid then uses interpreted
fallback, while Strict reports an error. No native System Profile JIT or AOT
target is produced, and the IR is not a serialized package ABI. Native lowering
is deferred until checked task-memory, primitive-authority, mutable-workspace,
and generation interfaces can be preserved by a second backend.

The module interpreter's explicitly typed `INT64`/`UINT64` subset does not
complete general BASIC64 integer semantics for ordinary programs or every
legacy/floating builtin combination. Unsupported conversions and 64-bit/f64
mixing fail rather than round. `FINALLY`, workspace migration, exhaustive
historical SWI error/X-bit behavior, and cryptographic package verification
remain open. The hosted dispatcher supports the common X-form error-block/V
convention described below, but this does not freeze System Profile's error ABI
or implement an OS error vector. Classic-compatible numeric programs retain their prior
Hybrid/Strict JIT paths.

The native `SystemModule` parser is profile-gated. Classic and Hybrid parsing
still uses the legacy source lexer; tests prove `TRY%`, `CATCH%`, and `PRIMITIVE%`
remain ordinary identifiers there. The new block/type syntax is not silently
enabled by setting `MODE=BASIC64` on an ordinary source file. Representative
Error and Wimp fragments remain executable unit-test fixtures in
`src/basic_compat/system_profile.rs`; FileSwitch is also the active bounded
channel module at [`../modules/FileSwitch.bas64`](../modules/FileSwitch.bas64).
The current capsule also loads [`../modules/Graphics.bas64`](../modules/Graphics.bas64),
which owns the public BASIC64 policy for `OS_Plot` and `OS_ReadPoint` while
retaining the existing Rust CPU-raster mechanisms. This is a partial WP5.5
ownership migration, not a complete graphics or Phase 5 delivery. It also loads
[`../modules/ColourTrans.bas64`](../modules/ColourTrans.bas64), which owns the
three existing name-only `ColourTrans_*` SYS services; no numeric SWI IDs are
published. The hosted HSV and SetGCOL paths are implemented, while
`ColourTrans_WritePalette` is an explicit no-op compatibility shim.
The capsule also publishes [`../modules/Wimp.bas64`](../modules/Wimp.bas64),
which owns `Wimp_Initialise`, `Wimp_CloseDown`, `Wimp_StartTask`,
`Wimp_OpenWindow`, `Wimp_CloseWindow`, `Wimp_GetWindowState`, and
`Wimp_SetExtent`. BASIC64 handles lifecycle and window-block decoding/result
policy; narrow Rust mechanisms repeat caller ownership and geometry checks and
commit atomic window/stack changes. Other Wimp SWIs remain on their prior
route, so this is a partial WP5.6 migration.
The Console example is the real source
module at [`../modules/Console.bas64`](../modules/Console.bas64). The original
Phase 4 capsule comprised seven native foundation modules; the current capsule
adds command/system modules and `Mos`/`FileSwitch`/`Graphics` for bounded BASIC64-owned
MOS and file policy. FileSwitch owns bounded OS_GBPB reasons 1–10 in
addition to the existing channel services and OS_File reasons 0–12, 16–18,
and 255. BASIC64 selects OS_File path-source policy and candidate order; Rust
provides checked runtime-variable/path-string access and guest-sandbox
candidate/load mechanisms. File$Path, R4 path-list, and path-variable reasons
are supported within the documented byte/candidate bounds and String /
LiteralString subset. Run$Path, macro expansion, wildcard search, and other
native path behavior remain explicit gaps. Create-existing and other hosted
FileSwitch also owns hosted OS_FSControl reasons 0, 1, 5–9, 11, 13, 14, 18,
19, 22, 25, 31, 33, 37, 39, 40, 43–45, and 50. BASIC64 selects register and
reason policy; Rust supplies checked strings, task-local directory/selection
state, bounded catalogue snapshots, and canonical-name/file-type mechanisms.
Reason 11 reports the recognized temporary HostFS selector and previous
selector while rejecting unsupported special fields atomically. Reason 37
supports bounded R3 String/LiteralString and R4 ordered path-list sources,
qualified-path bypass, and final-attempt canonicalization; general GSTrans and
macros remain unsupported. Reasons 7/8 normalize relative paths against the
library directory for both catalogue title and entries. Reason 50 changes the shared volume label. Other
FileSwitch deviations are explicit rather than native compatibility claims.
The source files are
[`../modules/System.bas64`](../modules/System.bas64),
[`../modules/Boot.bas64`](../modules/Boot.bas64),
[`../modules/Error.bas64`](../modules/Error.bas64),
[`../modules/ModuleManager.bas64`](../modules/ModuleManager.bas64),
[`../modules/TaskManager.bas64`](../modules/TaskManager.bas64),
[`../modules/Memory.bas64`](../modules/Memory.bas64), Console, and
[`../modules/Mos.bas64`](../modules/Mos.bas64).
The current capsule also loads
[`../modules/RicochetCommands.bas64`](../modules/RicochetCommands.bas64).
System is a source-visible startup facade and owns the bounded
`OS_SWINumberToString`, `OS_SWINumberFromString`, and `OS_ReadMonotonicTime`
services plus PRM-numbered `OS_ReadVarVal` (`&23`) and `OS_SetVarVal` (`&24`).
The variable SWIs use `SystemVariableStore`-gated Rust mechanisms over one
runtime-scoped guest store with checked caller logical buffers and task-local
opaque wildcard contexts. Only string type 0 and literal string type 4 are
supported. Type 0 immediately expands exact `<name>` references, quoted
strings, doubled quotes, and printable `|<`, `|>`, `||`, `|"` escapes; type 4
is raw. Substitution results are not rescanned. Unsupported/malformed syntax
and missing variables fail before mutation; no public `OS_GSTrans` contract is
claimed. The store is bounded to 128 entries,
32 KiB aggregate data, 32-byte visible-ASCII names, and 256-byte UTF-8 values.
It is not host environment access or persistent storage. A separate
`SystemVariableWrite` Task right is checked on the original requestor and
granted only to the trusted interactive MOS session; the module's provider
capability does not grant caller mutation authority. `RicochetCommands.bas64`
owns `*SET`, `*SHOW`, and `*UNSET` policy through these shared SWIs. Its
`StartupPolicy` capability separately limits configuration read and
MOS/desktop handoff to its declared protected primitives. `RicochetCommands` parses and presents
read-only `*INSPECT MODULES`, `MODULE`, `SWI`, and bounded retained-source
`DEFINITION` queries (`SOURCE` is a read-only alias). Classic module commands
are `*Modules`, `*RMLoad`, `*RMRun`, `*RMKill`, and conditional `*RMEnsure`;
known native ROM/RMA commands without a hosted state model are explicit
unsupported operations. It uses ModuleManager's shared caller-buffer query
SWIs for inspection, while mutations use the separate `OS_Module` service.
OS_CLI command names, display metadata, ordering, Help matching, and BASIC64
handler selection come from the live module command registry. Rust-backed
transitional commands have explicit, name-bound bridge descriptors owned by
`RicochetCommands`; there is no catch-all command fallback. Active module/SWI identity is explicitly public metadata;
retained definition source requires task-scoped `SourceRead`, and OS_Module
mutation requires separate `ModuleManagement`. The host explicitly bootstraps
the interactive MOS task with both rights; ordinary tasks receive neither and
do not inherit them by task ID or module-provider capability. Boot is a lifecycle-only policy module
with no host grant and imports System's typed functions. Rust consumes Boot's
selected request without reinterpreting the saved Language value. Boot depends
on Console, Error, Memory, ModuleManager, Mos, System, and TaskManager, so all seven
service providers are active before its `Start` hook runs. Console also has its
own startup hook; System and the narrow management/error/task/memory services
do not need startup hooks. The implemented System services remain a bounded
subset of the broader historical RISC OS System family.

The initial Error, ModuleManager, TaskManager, and Memory services are also
real source definitions rather than parser fixtures or empty modules. Error
owns `OS_GenerateError` and validates/reads the caller's error block through a
checked primitive before raising a structured error. The dispatcher recognizes
numeric X bit 17 and named `X` calls: success clears V; failure returns the
standard four-byte number/NUL-message block in a reserved caller-task logical
slot at R0 and sets V. Unknown SWIs use generic error code 1; structured
service failures retain their code. This X form avoids host pointers, but
normal calls still propagate through the existing hosted `RuntimeError` path
rather than a RISC OS error vector/handler. BBC `SYS ... TO ... ; flags` can
capture NZCV (V is bit 0) in this hosted subset.

`RicochetCommands` also owns the hosted six-key v3 `*CONFIGURE`/`*STATUS`
contract. The standard analogues are `Language` (only module 0/3) and
`WimpMode`/`Mode` (`Auto` or a supported `X<width> Y<height> C/G<depth>`
selector). WimpMode alone controls resolution and palette: Auto means
host-sized, full-colour C16M/Rgb888; fixed selectors choose both dimensions
and palette. Old `DisplayResolution` and `DisplayColour` file keys migrate to
WimpMode, v2 WimpMode takes precedence over retired `RicochetOutputProfile`,
and old `WindowFurniture` is discarded. These are not public options. There is
no bevelled furniture option or rendering path. See the MOS configuration
audit for defaults, mappings, and unsupported PRM mode selectors.

ModuleManager's `Ricochet_ModuleInfo` extension (&4FF10, ABI 1) enumerates active
module names, versions, and lifecycle states into checked caller memory.
ModuleManager also owns post-boot
`OS_Module` (&1E) reasons 1 Load and 4 Delete, plus `Ricochet_ModuleLookup`
(&4FF12), `Ricochet_SwiInfo` (&4FF13), `Ricochet_ModuleExport` (&4FF14), and
`Ricochet_DefinitionSource` (&4FF15). Load accepts bounded caller-path,
UTF-8 BASIC64 source of filetype `&064`; guests cannot request protected
capabilities, and public imports must resolve to active dependencies. A same-
title reload supports only the compatible-immediate class: it preserves module
and entry-cell identities/workspace, advances all exported SWI generations
atomically, and leaves active old-generation calls intact. It rejects lifecycle,
capability, dependency, public-contract, or complete persistent type/schema
changes rather than attempting state migration. Candidate Start and old
Quiesce/Finalise do not run. Delete uses transactional Quiesce/Finalise,
protects foundation and depended-on modules, and allows retry after failed
Finalise. `Ricochet_ModuleExport` enumerates active manifest SWI exports;
`Ricochet_DefinitionSource` reads retained current PROC/FN source in bounded
caller-memory chunks. `Ricochet_ModuleExport` is public metadata, while
`Ricochet_DefinitionSource` requires `SourceRead` before retained bytes are read
or returned. Its CLI projection has the same check. `RicochetCommands.bas64` owns
`*INSPECT` query presentation and the documented classic module command subset;
remaining transitional MOS behavior is reached only through explicit
`RicochetCommands` bridge descriptors, not a separate Rust command table.
Other historical `OS_Module` reasons are structured rejections, particularly reason 18 because
it returns process pointers; `%` instantiations and native `&FFA` images are
unsupported. The project queries return manifest identity through checked
caller buffers, never pointers. Exact OS_Module reasons and register shapes
are documented in [`ricochet-boot-capsule.md`](ricochet-boot-capsule.md) and the
[RISC OS PRM](https://www.riscos.com/support/developers/prm/modules.html).

TaskManager's
`Ricochet_TaskInfo` extension (&4FF11, ABI 1) reports the caller task ID, logical
address-space span, and dynamic-area count, not task-creation or scheduler
control. Memory owns standard `OS_ChangeDynamicArea` (&2A) and
`OS_DynamicArea` (&66) reason dispatch. Its hosted areas use automatic
caller-local IDs/bases and checked task memory, with a 16 MiB per-area cap,
32 MiB per-task reservation cap, and no callbacks, physical pages, or
doubly-mapped support. Exact public ownership and compatibility gaps are listed
in [`ricochet-boot-capsule.md`](ricochet-boot-capsule.md) and
[`ricochet-compatibility-matrix.md`](ricochet-compatibility-matrix.md).

The named-only `RICOCHET_DESKTOP` and `RICOCHET_DISPLAY` services are routed
through `DesktopServices.bas64::DESKTOPSERVICE` and
`DisplayManager.bas64::DISPLAYSERVICE`. The closed dispatcher mapping does not
invent numeric SWI IDs. Module imports grant only their narrow host mechanisms;
display apply checks the original caller's `ConfigurationWrite` capability
independently.

## Purpose

Ricochet should not encode its module system, primitive boundary, capabilities,
and live identities as strings, magic integers, parallel arrays, or informal
register conventions merely because BASIC64 has not yet acquired appropriate
syntax.

The System Profile adds the smallest language facilities required to write
clear, safe, inspectable operating-system modules while preserving the direct
character of BBC BASIC.

The design test is:

> **Make invalid system code difficult to express while preserving the
> directness and readability of BBC BASIC.**

The profile is requirements-driven. It is not permission to design an entire
general-purpose language before implementing Ricochet.

## Compatibility boundary

Classic BBC BASIC and native BASIC64 share a frontend and runtime where
semantics allow, but their profiles are explicit.

Native system modules declare the BASIC64 profile, for example:

```basic
REM @BASIC64 MODE=BASIC64
```

Existing `CLASSIC` and `HYBRID` programs must not reinterpret established
identifiers or syntax because the system profile adds keywords. Profile choice,
language semantic version, target, and relevant dependency versions are part of
parsed-program and compiled-artifact identity.

## System Profile 0.1 feature set

### Modules and namespaces

System Profile 0.1 uses comment directives for manifest metadata so old BASIC
lexers do not gain new keywords. The parsed source must begin with the profile
and module identity; metadata precedes executable statements. A module name and
version form its namespace identity. Public names are explicit (`@EXPORT`),
except `@SWI` definitions, which become public automatically; unlisted
definitions stay module-local. This is the supported source shape:

```basic
REM @BASIC64 MODE=BASIC64
REM @SYSTEM_PROFILE 0.1
REM @MODULE FileSwitch 1.0.0
REM @CAPABILITY FileSystem
REM @CAPABILITY RuntimeErrors
REM @IMPORT Host.FileChannel.Open FileSystem
REM @IMPORT Host.FileChannel.Close FileSystem
REM @IMPORT Host.FileChannel.ReadByte FileSystem
REM @IMPORT Host.FileChannel.WriteByte FileSystem
REM @IMPORT Host.FileChannel.ReadPosition FileSystem
REM @IMPORT Host.FileChannel.SetPosition FileSystem
REM @IMPORT Host.FileChannel.ReadExtent FileSystem
REM @IMPORT Host.FileChannel.SetExtent FileSystem
REM @IMPORT Host.FileChannel.CanonicalNameLength FileSystem
REM @IMPORT Host.FileChannel.Args7SpareBytes FileSystem
REM @IMPORT Host.FileChannel.WriteCanonicalName FileSystem
REM @IMPORT Host.Runtime.UnsupportedServiceReason RuntimeErrors
REM @SWI OS_Find &0D FindService REGISTERS=R0:U32:INOUT|R1:U32:IN|R2:U32:IN;BLOCKING=FALSE;REENTRANT=FALSE
REM @SWI OS_BGet &0A ByteGetService REGISTERS=R0:U32:INOUT|R1:U32:IN;CARRY=OUT;BLOCKING=FALSE;REENTRANT=TRUE
REM @SWI OS_BPut &0B BytePutService REGISTERS=R0:U32:IN|R1:U32:IN;BLOCKING=FALSE;REENTRANT=TRUE
REM @SWI OS_Args &09 ArgsService REGISTERS=R0:U32:INOUT|R1:U32:IN|R2:U32:INOUT|R5:U32:INOUT;BLOCKING=FALSE;REENTRANT=FALSE
```

The current `modules/FileSwitch.bas64` is the executable bounded example for
these channel services, GBPB reasons 1–10, and the bounded OS_FSControl subset
above. Its direct-path channel subset
supports open/close, byte I/O, Args reasons 0–5 and 7, checked canonical-name
output, bounded block transfers, bounded directory records, and selected
FSControl policy. It does not imply support for GBPB reasons 11–12, other FileSwitch families, or BASIC
`OPENIN`-style statements, which are not currently implemented by the hosted
BASIC runtime.

Supported directives are `@MODULE name major.minor.patch`, `@COMMAND name
Commands|FileCommands PROC handler "syntax" "description"`, `@IMPORT_MODULE name
minimum-version` (with `@DEPENDS name minimum-version` as a spelling alias),
`@IMPORT_SYMBOL
module PROC|FN symbol`, `@CAPABILITY name`, `@IMPORT primitive capability`,
`@SWI name number proc [semicolon-delimited-contract]`, `@EXPORT`/`@PRIVATE
PROC|FN name`, `@STATE name TYPE [READONLY]`, the three `@LIFECYCLE` hooks,
and `@REPLACE IMMEDIATE|QUIESCENT|MIGRATING|RESTART`. The dependency spellings
have identical semantics; declare a given dependency exactly once. Each import
requires a requested capability and a host grant at link time. `@IMPORT_SYMBOL`
is validated against a declared dependency and provider export; qualified calls
execute only when that linked import resolves to an active provider export.
There is no `MODULE`/`END MODULE` block syntax, textual include, or general
qualified-name resolver outside declared symbol imports in 0.1.

`@COMMAND` publishes an ordered command descriptor together with the module:
name, category, handler kind/identity, syntax, and Help description. Its PROC
handler is a private owner-local definition with one typed `STRING` parameter;
declaring a command does not add that handler to `@EXPORT` or make it
importable. `BRIDGE` is accepted only for the closed command allowlist in the
trusted `RicochetCommands` capsule module. The live registry is sorted by
case-folded module title, then by source declaration order; command execution
uses the first matching final-dot prefix, while `*HELP prefix.` displays every
matching descriptor. The nonempty `command.*` manifest rows are backward-
readable by this runtime and preserve the old canonical bytes when absent, but
older strict v1 decoders reject the extension, so this is not forward-reader
compatibility.

`@SWI` register kinds currently include `BYTE`, `U32`, `S32`, `ADDRESS32`, and
`HANDLE<Type>` with `IN`, `OUT`, or `INOUT` directions. Optional attributes
include `REGISTERS=...`, `MEMORY=R0:READ|WRITE|READWRITE:byte-limit`, `PC=...`,
`CARRY=...`, `BLOCKING=TRUE|FALSE`, `REENTRANT=TRUE|FALSE`, and `ERROR=name`.
Attributes are separated by semicolons. Registry validation rejects duplicate
registers, invalid directions/widths, ambiguous PC register ownership, and
memory contracts without a compatible checked U32/ADDRESS32 pointer.

### Named records

Records provide structured values with named, typed fields:

```basic
RECORD FileInfo
    LoadAddress AS UINT32
    ExecAddress AS UINT32
    Length      AS UINT64
    Attributes  AS FileAttributes
    FileType    AS UINT16
END RECORD
```

Ordinary records are managed values. They do not imply object identity,
inheritance, dynamic dispatch, or a fixed binary layout. A binary overlay or
packed external representation, if supported, must be requested explicitly and
must still obey checked logical-memory rules.

### Enumerations and flags

Named enumerations and bit flags replace magic numeric constants while
preserving explicit external values:

```basic
ENUM FileReason AS UINT32
    Save      = 0
    WriteInfo = 1
    Load      = 255
END ENUM

FLAGS FileAccess AS UINT32
    Read   = 1
    Write  = 2
    Create = 4
END FLAGS
```

Conversion from untrusted register or memory values is checked. Unknown external
values must be representable or rejected according to the declared public
contract; they are not silently assumed valid.

### Typed procedures and functions

Native BASIC64 supports explicit parameter and result types while retaining
traditional suffixes where convenient:

```basic
DEF FN KeepOpen(channel AS HANDLE<FileHandle>) AS HANDLE<FileHandle>
= channel

DEF PROC SaveFile(channel AS HANDLE<FileHandle>) THROWS FileError
    THROW FileError, 73, "file not found"
ENDPROC
```

The profile requires:

- typed parameters;
- an explicit result type for functions;
- procedure semantics without a fabricated result;
- an explicit structured failure type with `THROWS ErrorType`;
- checked conversion at public register and memory boundaries.

Function result types are mandatory and are checked when the function returns.
The small interpreted type set is `BYTE`, `UINT16`, `UINT32`, `INT32`, `UINT64`,
`INT64`, `ADDRESS32`, `STRING`, declared record/enum/flags/error names, and
`HANDLE<Name>`. Integer and enum/flag representation limitations are recorded
in the checkpoint above; no hidden coercion turns handles or addresses into
ordinary numbers.

### Structured errors

System code uses structured errors internally rather than hidden global state or
ad hoc sentinel values:

```basic
ERROR FileError
    Code AS UINT32 READONLY
    Message AS STRING READONLY
END ERROR

DEF PROC Open THROWS FileError
    TRY
        PROC ValidateName
    CATCH failure AS FileError
        THROW FileError, failure.Code, failure.Message
    ENDTRY
ENDPROC
```

The interpreter propagates a structured failure to the caller or matching
`CATCH`. At the public SWI boundary, numeric X bit 17 and named `X` calls now
clear V on success or return normally with V set and R0 pointing to a
caller-task-local, checked standard error block on failure. The block contains
a 32-bit code and NUL-terminated message in reserved logical memory, not a host
pointer. Unknown SWIs use generic code 1; structured service failures retain
their code. The PRM-specific `XOS_GenerateError` form instead preserves its
input R0 error-block address and sets V. Non-X calls still use the hosted `RuntimeError` propagation path;
RISC OS error vectors/handlers and exhaustive service-specific error/flag
mapping are not implemented. BBC `SYS ... TO ... ; flags` exposes the hosted
NZCV result (V is bit 0). Rust primitives do not fabricate unchecked guest
pointers.

System Profile 0.1 uses typed throws and structured `TRY`/`CATCH`/`ENDTRY`.
Thrown values carry the declared type, code, message, and readonly fields;
catch matching is by declared error type. Unwinding restores procedure
bindings and loop depth before entering the catch. `FINALLY`, result-like error
values, and mapping every historical SWI error/X-bit register convention are
not implemented, so 0.1 syntax remains provisional rather than frozen.

### Managed opaque handles

Opaque types currently provide nominally checked identities at the BASIC64
boundary:

```basic
HANDLE FileHandle
HANDLE TaskHandle
HANDLE WindowHandle
HANDLE DefinitionHandle
```

`HANDLE<Name>` values cannot be arithmetically manipulated or coerced to a
number/address, and an SWI contract can preserve their nominal type across
registers and function results. Resource-aware primitive descriptors declare
rights for opaque-handle arguments; the call path checks that the identity is
registered, has the declared type, belongs to the invoking task, and carries
the requested right before use. The in-slice resource manager demonstrates this
with checked buffer reads; richer resource-specific managers remain future
work. A handle is never a host pointer or a caller logical address.

The type system distinguishes at least:

```basic
address AS ADDRESS32      : REM caller-scoped logical address
file    AS HANDLE<FileHandle> : REM opaque identity; validate with its owner
task    AS HANDLE<TaskHandle> : REM opaque task identity
```

Compatibility profiles may continue to expose numeric registers and logical
addresses where required. Native modules use managed handles whenever the
public contract does not explicitly require a numeric address or handle.

### Read-only bindings and fields

The executable subset supports `@STATE name TYPE READONLY` and individual
`READONLY` record/error fields. State can be initialized once by module startup
or the first invocation; later assignment fails. Read-only fields are enforced
when assigning through a record path. `LET READONLY name AS TYPE = expression`
adds a routine-scoped immutable binding in PROC and FN bodies. General
compile-time constants and deep functional immutability remain outside this
subset.

### SWI and primitive metadata

The current executable form places metadata in ordinary BASIC `REM` comments,
then retains a link from the SWI declaration to the defining procedure:

```basic
REM @CAPABILITY ConsoleOutput
REM @IMPORT Host.Console.WriteByte ConsoleOutput
REM @SWI OS_WriteC &00 WriteC REGISTERS=R0:U32:IN
DEF PROC WriteC
    PRIMITIVE Host.Console.WriteByte, R0% AND &FF
ENDPROC
```

There is no executable `@SWI_ALIAS` syntax yet. The source is parsed into one
resolved manifest representation, and source inspection/reflection leads from
an SWI export to its implementing definition. Primitive imports and requested
capabilities are linked; a BASIC64 definition cannot invoke an undeclared or
ungranted primitive by guessing its name.

### Representative executable fragments

These fragments use the tested 0.1 spelling and reflect the fixtures in
`src/basic_compat/system_profile.rs` (the Console module is maintained as a
standalone source file):

```basic
REM @BASIC64 MODE=BASIC64
REM @SYSTEM_PROFILE 0.1
REM @MODULE FileSwitch 1.0.0
REM @SWI FileSwitch_Info &100 ReadInfo
ERROR FileError
    Code AS UINT32 READONLY
    Message AS STRING READONLY
END ERROR
RECORD FileInfo
    Length AS UINT64 READONLY
    Access AS FileAccess
END RECORD
ENUM FileReason AS UINT32
    NotFound = 73
END ENUM
FLAGS FileAccess AS UINT32
    Read = 1
    Write = 2
END FLAGS
HANDLE FileHandle
DEF FN FileLength(info AS FileInfo) AS UINT64
= info.Length
DEF PROC Fail THROWS FileError
    THROW FileError, 73, "file not found"
ENDPROC
```

The Error fixture also verifies typed catch/unwind and rejected undeclared
throws. The Wimp fixture proves that a typed window-handle register cannot be
assigned to an ordinary numeric result, while same-typed handles round-trip:

```basic
REM @SWI Wimp_HandleTest &101 Entry REGISTERS=R0:HANDLE<WindowHandle>:INOUT|R1:U32:OUT
HANDLE WindowHandle
ERROR TypeError
    Code AS UINT32 READONLY
    Message AS STRING READONLY
END ERROR
DEF PROC Entry
    TRY
        R1% = R0%
    CATCH fault AS TypeError
        R1% = fault.Code
    ENDTRY
    R0% = FN Echo(R0%)
ENDPROC
DEF FN Echo(window AS HANDLE<WindowHandle>) AS HANDLE<WindowHandle>
= window
```

The complete checked Console source additionally demonstrates `ADDRESS32`
memory traversal, six SWI contracts including tagged `OS_ReadLine` options,
capability imports, and lifecycle hooks. `modules/FileSwitch.bas64` is now an
active owner of the bounded four-SWI channel subset documented above. Wimp
owns the lifecycle trio plus OpenWindow, CloseWindow, GetWindowState, and
SetExtent; remaining Wimp services stay on their earlier hosted route.

### AST, interpreter, portable IR, and JIT boundary

Profile constructs have source-located AST/runtime representations: named type
definitions, typed parameter/result/error tables, typed `Value` variants, and
explicit `PrimitiveCall`, `Try`, `Catch`, `EndTry`, and `Throw` statements.
Name/type/visibility validation runs before registry linking; the interpreter
is the reference semantics for checked parameters/results, records, flags,
handles, readonly fields/state, errors, and caller-scoped memory. Reflection
preserves the resolved source/type/export metadata without adding authority.

The separate portable typed high-level IR is implemented as
`PortableSystemIr`. It carries the validated manifest, module and definition
identity, source locations, type/workspace schemas, visibility, direct
dependency versions, and typed operations. Its statement, expression, place,
and control-flow payload is explicit IR data; it stores no parser-AST payload.
The reference interpreter prepares the IR and reconstructs the shared BASIC
statement program from it. The Hybrid/Strict JIT boundary invokes the same IR
builder; typed System Profile operations are explicitly rejected there, so
Hybrid falls back and Strict reports an error before native execution. AOT is
also rejected. No System Profile native JIT/AOT is claimed, and the high-level
IR is not yet a serialized package ABI or standalone bytecode. This is
deliberately stronger than silently compiling only numeric statements and
skipping types, catches, capabilities, or address checks.
The conservative admission rule also sends standalone `MODE=BASIC64` programs
to the interpreter, even when they contain no typed declarations, because
integral literals there may retain exact values that an f64 compiler would
silently round. CLASSIC/HYBRID literals keep their legacy floating semantics.

These are the typed IR/interpreter semantics and remaining ownership rules;
known gaps are called out explicitly rather than treated as implemented:

| BASIC64 construct | IR-level meaning that must be preserved |
|---|---|
| Typed parameter/result | A call boundary carries the declared type and performs checked input/result validation; a failed check is an explicit runtime error with its source location. |
| Record/enum/flags | Record fields are schema-indexed managed values; enum/flag operations retain nominal type identity, and bitwise flag operations reject mixed flag types. |
| Opaque handle | A typed identity passes unchanged through typed calls/registers; arithmetic/numeric casts are invalid. Resource services register handles with owner/type/rights; the primitive call checks them at use. |
| `ADDRESS32` | An operand contains caller/task identity plus logical offset; offset arithmetic checks 32-bit overflow and each load/store carries caller memory bounds. Such values cannot enter persistent state or serialized module data. |
| `READONLY` | Stores after initialization or to a readonly field are explicit validation failures, not optimizable-away writes. |
| `THROW` / `CATCH` | Throw is a typed control-flow exit; catch regions include their error type and unwind depth, restoring call/loop bindings before entering the matching handler. |
| `PRIMITIVE` | Link-resolved primitive identity and capability are explicit operands; register marshalling and caller context remain checked. |

IR instruction locations map back to source lines; the manifest identity and
the live registry's invocation plan supply module/source, transitive dependency,
target, language-profile, runtime-ABI, and definition-generation identity to a
derived target. A JIT/AOT backend may lower a row only when it implements its
invariant; the current compiled paths return an explicit unsupported-feature
result and the reference interpreter remains the only module backend.

### Managed references and logical addresses

The current source AST and interpreter distinguish:

- managed BASIC values;
- managed references;
- nominal opaque handles (resource authority is checked by the owner);
- caller-scoped logical addresses;
- explicitly shared logical-memory views (not yet exposed in System Profile
  0.1);
- private native primitive values that cannot escape into BASIC64.

`ADDRESS32` values carry the invoking task identity at runtime, support only
checked 32-bit integral offset arithmetic, and may be read/written only through
that task's bounds-checked memory. They cannot be stored in private persistent
module state or nested record state. Public U32 memory pointers are wrapped as
caller-scoped addresses when entering typed System Profile definitions. No cast
or generic numeric operation turns a handle or address into an ordinary number.
The portable typed IR encodes those addresses as 32-bit checked operations
owned by the invoking task; the reference adapter delegates bounds enforcement
to the same checked task-memory runtime used by BASIC64.

## Structures are not objects

System Profile 0.1 deliberately separates:

- **records** for structured values;
- **handles** for nominal resource identity, with authority checked by the
  owning service;
- **modules** for behaviour, namespace, and owned state;
- **procedures/functions** for executable definitions.

This combination is sufficient to test the first Ricochet modules. Records do
not acquire identity merely because they contain fields, and handles do not
imply a conventional class hierarchy.

## Deferred language features

The following do not block System Profile 0.1:

- classes and inheritance;
- universal object or message semantics;
- operator overloading;
- user-defined generics;
- algebraic data types and pattern matching;
- `async`/`await` or actor syntax;
- macros and compile-time metaprogramming;
- extension methods;
- universal late-bound calls;
- persistent objects as a language primitive;
- deep functional immutability.

First-class functions and lexical closures are likely useful. They should follow
the first module slice unless a concrete lifecycle, callback, or service-handler
requirement proves they are necessary earlier.

Deferred features must be justified by executable Ricochet requirements rather
than general language fashion.

## Implementation obligations

System Profile 0.1 is not complete when only its parser accepts new syntax. Each
accepted feature needs:

- profile-gated lexical and grammar rules;
- AST representation with source locations;
- name and visibility resolution;
- type checking and useful diagnostics;
- shared IR representation where executable;
- interpreter semantics as the reference behaviour;
- reflection metadata needed by the live registry;
- serialisation/cache identity where applicable;
- explicit JIT handling: native lowering or a safe interpreter/runtime boundary;
- unit, negative, profile-compatibility, and end-to-end module tests;
- documentation and small readable examples.

The JIT need not lower every new construct in the first release. Unsupported
definitions may remain interpreted, but compiled paths must reject or cross an
explicit checked boundary rather than silently changing semantics.

## Preparatory design and implementation sequence

The Phase 0–3 slice has completed the first source-design loop: representative
Console, Error, FileSwitch and Wimp fragments exist; the provisional comment
metadata and type/signature grammar is exercised; compatibility gates and
interpreter semantics are tested; and Console is routed through a module-owned
SWI subset. The initial syntax should now be reviewed for awkwardness before
System Profile 0.1 is not declared stable; its grammar and package ABI remain
provisional pending broader review and completion of the listed acceptance
criteria.

The first 0.1 design loop has resolved and implemented local immutable
bindings, handle-registry rights, exact explicitly typed 64-bit integers, and
executable qualified imported PROC/FN calls. Common X-form SWI errors now use
a checked per-task standard error block and V flag; exhaustive public SWI and
normal error-handler mapping remains open. The portable IR is complete for the admitted profile
subset and is the interpreter boundary; native System Profile JIT/AOT lowering
remains future work until module state, checked memory, primitive authority,
and generation interfaces are stable enough to share with a second backend.
The grammar, serialized package/IR format, and language semantic version should
be frozen only after broader compatibility review.

The Console source remains the primary usability test, not proof that all
System Profile requirements or compiler exit criteria are complete.

## Acceptance criteria for System Profile 0.1

- A native BASIC64 module has an explicit namespace, imports, exports, private
state, records, enums/flags, typed definitions, structured errors, and opaque
handles without encoding them as naming conventions.
- The same parser still accepts representative BBC BASIC V/VI fixtures under
  their compatibility profiles with unchanged meaning.
- A system module cannot pass a `FileHandle` where an `ADDRESS32` is required or
  turn either into a host pointer.
- Primitive imports fail at link time when the required capability is absent.
- Qualified PROC/FN imports execute only after provider visibility and import
  identity have been linked; provider workspace changes are transactional.
- Opaque resource handles are checked for identity, type, caller ownership, and
  declared rights at the primitive use site.
- Typed `INT64`/`UINT64` arithmetic does not round through `f64`; unsupported
  floating conversions fail explicitly.
- Read-only locals are scoped and restored for both PROC and FN calls.
- The interpreter executes a reconstruction from complete typed IR operations;
  compiled backends reject unsupported native lowering explicitly.
- SWI metadata resolves to one owning module and definition identity.
- Source locations and type information remain available through the live
  registry and interpreted execution.
- Unsupported JIT constructs fall back or fail explicitly without semantic
  drift.
- The Console module remains concise and recognisably BASIC rather than becoming
  a framework-heavy systems program.

### Checkpoint disposition

| Area | Phase 0–3 status |
|---|---|
| Metadata grammar, versioned manifest, source locations, definition/export reflection | Implemented and tested as the current 0.1 executable subset. |
| Records, enums/flags, typed parameters/results, structured catch/throw, nominal handles, read-only state/fields/locals | Interpreter semantics and negative/type-boundary tests are implemented. PROC and FN local bindings are scoped per invocation. |
| Caller-scoped addresses and memory safety | Typed for System Profile SWI parameters/PC, checked on memory access and arithmetic, rejected from persistent state. Legacy numeric BASIC memory access remains for compatibility. |
| CLASSIC/HYBRID compatibility | Profile-gating tests keep System Profile names ordinary under the compatibility parser. The Hybrid JIT keeps the numeric classic path and routes the new features to interpreter; Strict rejects them explicitly. |
| Portable typed IR and module JIT/AOT | A separate source-located typed IR fully represents the admitted statement/expression subset without retaining parser AST payload and is reconstructed into the reference interpreter. It carries definition/workspace/source identity and checked task-owned address operations. Hybrid/Strict/AOT requests share an explicit boundary; native System Profile lowering and serialized package ABI are not implemented. |
| External SWI structured-error mapping and exhaustive BBC error/X-bit conformance | Bounded X-form mapping is implemented: success clears V; errors return a caller-task standard block in R0 and set V; unknown SWIs use code 1. Non-X failures retain hosted `RuntimeError` propagation. Error vectors/handlers and exhaustive SWI-specific behavior remain unimplemented. |
| State migration, broad exact integer semantics outside explicitly typed System Profile values, exhaustive SWI error/X-bit conformance, general constants, cryptographic package verification | Not implemented; the current executable subset does not imply those broader guarantees. |
