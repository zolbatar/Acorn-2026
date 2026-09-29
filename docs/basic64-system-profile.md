# BASIC64 System Profile

## Status

This document records the agreed language direction and the provisional,
executable grammar for the native BASIC64 profile used to implement Trellis
modules and system policy.

The feature categories and semantic boundaries are firm. The spelling below has
now been exercised by the Console, Error/FileSwitch and Wimp test fragments, but
remains System Profile 0.1 syntax under review rather than a frozen long-term
language ABI.

This profile is an additive native language. It does not silently change BBC
BASIC V/VI compatibility behaviour.

## Executable subset checkpoint — 2026-09-29

The first vertical slice now exercises more than parser-only syntax:

- `REM @SYSTEM_PROFILE 0.1` modules declare versioned dependencies, symbol and
  primitive imports, requested capabilities, SWI contracts, lifecycle hooks,
  replacement policy, private state, and public/private symbols. A deterministic
  `TRELLIS-MANIFEST\t1` wire form round-trips the resolved manifest and rejects
  missing, duplicate, unknown, or invalid fields.
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
remain open. Classic-compatible numeric programs retain their prior
Hybrid/Strict JIT paths.

The native `SystemModule` parser is profile-gated. Classic and Hybrid parsing
still uses the legacy source lexer; tests prove `TRY%`, `CATCH%`, and `PRIMITIVE%`
remain ordinary identifiers there. The new block/type syntax is not silently
enabled by setting `MODE=BASIC64` on an ordinary source file. Representative
Error, FileSwitch, and Wimp fragments are executable unit-test fixtures in
`src/basic_compat/system_profile.rs`; the Console example is the real source
module at [`../modules/Console.bas64`](../modules/Console.bas64).

## Purpose

Trellis should not encode its module system, primitive boundary, capabilities,
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
general-purpose language before implementing Trellis.

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
REM @IMPORT_MODULE Runtime 1.0.0
REM @IMPORT_SYMBOL Runtime PROC OpenChannel
REM @CAPABILITY FileSystem
REM @IMPORT Host.File.OpenChannel FileSystem
REM @LIFECYCLE START Start
REM @LIFECYCLE QUIESCE Quiesce
REM @LIFECYCLE FINALISE Finalise
REM @REPLACE QUIESCENT
REM @STATE OPENCOUNT% UINT32
REM @EXPORT PROC Open
REM @PRIVATE PROC ValidateName
REM @SWI OS_Find &0D Find REGISTERS=R0:U32:INOUT|R1:U32:IN;BLOCKING=FALSE
```

Supported directives are `@MODULE name major.minor.patch`, `@IMPORT_MODULE name
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
`CATCH`. Mapping that value to each public SWI's documented RISC OS error block
and X-bit/register convention is not implemented yet; the Console slice retains
its current Rust `Result` error transport. Rust primitives do not fabricate
unchecked guest pointers.

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
capability imports, and lifecycle hooks. FileSwitch and Wimp remain test
fragments, not migrated public service modules.

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

This combination is sufficient to test the first Trellis modules. Records do
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

Deferred features must be justified by executable Trellis requirements rather
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
System Profile 0.1 is declared stable.

The first 0.1 design loop has resolved and implemented local immutable
bindings, handle-registry rights, exact explicitly typed 64-bit integers, and
executable qualified imported PROC/FN calls. Exhaustive public SWI error/X-bit
mapping remains open. The portable IR is complete for the admitted profile
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
| External SWI structured-error mapping and exhaustive BBC error/X-bit conformance | Not implemented; current errors propagate through the hosted Rust `Result` path. |
| State migration, broad exact integer semantics outside explicitly typed System Profile values, exhaustive SWI error/X-bit conformance, general constants, cryptographic package verification | Not implemented; the current executable subset does not imply those broader guarantees. |
