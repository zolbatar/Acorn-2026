# Trellis implementation phases and work packages

## Purpose

This document turns the decisions in
[`trellis-architecture.md`](trellis-architecture.md) into an incremental delivery
plan. Each phase leaves Trellis runnable and testable. Public SWI behaviour is
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
| WP0.3 | `REM @...` source metadata and deterministic `TRELLIS-MANIFEST\t1` serialization cover module identity/version, source identity, language/target profile, dependencies, symbol imports/exports, primitive imports, requested capabilities, SWI contracts, lifecycle hooks, and replacement policy. Validation rejects malformed schema rows, duplicate fields/exports/imports/hooks, invalid paths/contracts, unresolved link metadata, and imports without requested/granted capabilities. The Console module needs no manifest exception. | The schema is a strict internal tab-separated wire format, not a standard YAML/JSON package format; inventory generation from live dispatcher declarations and cryptographic source/package authenticity remain later work. `MIGRATING`/`RESTART` policies are metadata only; only compatible live replacement is implemented. |
| WP0.4 | The provisional System Profile 0.1 constructs in this slice have executable interpreter semantics and a separate source-located typed IR: records, enums/flags, typed signatures/results, structured errors, opaque handles, read-only module/record/local bindings, imported PROC/FN calls, managed-resource rights, and exact INT64/UINT64 literals, arithmetic, comparisons and loops. The IR preserves module/version/source/dependency identity, definition signatures, type/workspace schema, and checked caller-task address/memory semantics. CLASSIC/HYBRID gates and Console, Error, FileSwitch and Wimp fragments are tested. | No native System Profile JIT/AOT lowering or serialized IR/package ABI exists. Unsupported compiled requests fail at the shared admission boundary; interpreter semantics remain authoritative. Exhaustive historical SWI error/X-bit conformance and authenticated source/package verification remain open. |
| WP1.1 | Opaque IDs exist for modules, instances, definitions, generations, SWI cells, capabilities, and primitives. | IDs are process-local and are not persistent, by design. |
| WP1.2 | Versioned SWI cells retain active old generations; a real threaded replacement test verifies old-call completion and new-call acquisition. The cell retains weak retirement metadata; the active invocation lease owns the strong reference, so the old generation is freed as the last lease exits. | Weak retirement metadata is pruned during inspection/collection or the next replacement. |
| WP1.3 | Validation, linking, atomic publication, start, quiescence, and retirement are separate registry transitions. Failed start removes the module's entire SWI set before returning to `Linked`; retry is possible, and active calls retain their generation. | Hook execution is supplied by the BASIC64 module-management path in WP2.3. State migration remains a future contract; lifecycle failure diagnostics are returned to the manager but not retained as a structured registry record. |
| WP1.4 | Typed primitive descriptors resolve at link time; imports, grants, and active-module invocation are enforced. Console VDU/graphics stream parsing is behind the distinct `Host.Graphics.AcceptByte` mechanism, separate from raw `Host.Console.WriteByte`. | Primitive descriptors are register-shaped rather than a general typed native-call ABI; broader policy/mechanism extraction proceeds with later SWI owners. |
| WP1.5 | Public read-only reflection resolves definitions, exports, types, source locations, and manifests; Rust diagnostics trace an entry cell to owner, definition, source path, and generation. | No user-facing inspector is part of these phases. Reflection intentionally grants no edit or capability authority. |
| WP2.1 | Each source unit has its own namespace, visibility/import/export validation, private persistent workspace schema, type metadata, retained source locations, executable cross-module PROC/FN imports, and a distinct portable typed IR. The IR contains structured operations rather than a saved parser-AST payload and can reconstruct the common reference-interpreter program. Same-named private definitions coexist. | The IR is not a serialized package ABI, and native compilation is unsupported. Type checking and execution share the interpreter's authoritative semantics through an explicit checked IR adapter. |
| WP2.2 | Primitive calls execute in the interpreter and retain active module, caller task, task memory, and capability context. | Register-oriented prototype only; no generalized typed return values or shared IR lowering. |
| WP2.3 | BASIC64 `Start`, `Quiesce`, and `Finalise` hooks run through a trusted Rust-side module manager against private persistent state. Tests prove state survives calls/hooks, is shared through live replacement, is removed on retirement, and a throwing `Start` unpublishes every export. A throwing `Quiesce` restores the prior workspace and reopens admission; a throwing `Finalise` restores workspace state, leaves the module quiesced with exports inaccessible/source retained, and permits repair/retry. Retirement is rejected before `Finalise` unless quiescence succeeded. | State migration between incompatible workspace schemas is rejected, not implemented. Lifecycle workspace transactions cannot undo irreversible host effects from a primitive; hooks must defer them until success. Guest code cannot manage modules or call these hooks as authority-bearing management operations. |
| WP2.4 | Invocation plans distinguish `Interpreter`/`Jit`/`Aot` with module-version, source-path/hash, transitive dependency path/hash/version, profile, target, runtime-ABI, definition, and generation identity. Unsupported module JIT/AOT requests fail explicitly. Replacement invalidates derived targets while retained active generations finish. System Profile IR and JIT admission share one checked lowering boundary. | No compiled System Profile module target is produced or cached. Source fingerprints use non-cryptographic FNV-1a change detection; native lowering and authenticated package inputs remain future work. |
| WP3.1 | Numeric dispatch consults module-owned SWIs first and retains a diagnostic transitional route for remaining handlers. | Existing Rust fallback implementations and hard-coded numeric constants remain migration scaffolding; full public-surface migration is Phase 5. |
| WP3.2 | `Host.Graphics.AcceptByte` contains the hosted VDU stream parser/display policy; `Host.Console.WriteByte` is raw host byte output. `Host.Console.ReadByteStatus` supplies input state. Every call is module/capability gated. | The hosted VDU mechanism is still a Rust primitive and is not a separately replaceable BASIC64 graphics module. |
| WP3.3 | All six Console exports, including `OS_ReadLine`, are interpreted BASIC64 definitions with checked caller memory where applicable. Focused tests cover editing, accepted ranges, echo-only and R4 substitution, full-buffer bell, Escape, EOF, Control-D, CR/LF, and memory-bound behavior. | The embedded source is installed by the Rust dispatcher constructor; Rust legacy handler branches remain fallback code. Exhaustive historical ReadLine option combinations and exact error-block cases are not verified. Phase 4 boot-capsule loading and empty-table startup are not implemented. |
| WP3.4 | The public host-side `Basic64ModuleManager` accepts, rejects, and restores a source replacement without restarting; it preserves the entry cell, invalidates derived targets, and old invocation leases retain active generations. Its authority token is private and absent from guest APIs. | This is a trusted in-process management API, not yet a BASIC64 system browser/editor or user-facing authorization workflow. |

The runnable demonstration is in [`trellis-milestone-0-3.md`](trellis-milestone-0-3.md);
implementation evidence and compatibility commands are summarized in
[`trellis-compatibility-matrix.md`](trellis-compatibility-matrix.md). Phases 4
and later remain unstarted: this checkpoint is not the zero-public-SWI
bootstrap, does not migrate the entire SWI surface, and does not install a boot
capsule.

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
- proposed owning Trellis module;
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

**Exit criterion:** manifests publish the initial namespace atomically and
module start definitions run only after publication.

### WP4.4 — Native recovery surface

Add a deliberately restricted recovery path for capsule validation, linking, or
foundation-start failure. It displays the failed stage, module/definition,
structured error, ABI versions, and diagnostic log, and permits retry, alternate
capsule selection, or exit.

It provides no ordinary command line, user-file access, or public SWIs.

**Exit criterion:** corrupt and ABI-incompatible capsules fail diagnostically
without entering a partially initialised Trellis environment.

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

### WP5.2 — Memory and task services

Migrate logical-memory, dynamic-area/shared-region, task identity, scheduling,
event, and IPC services. Keep allocation, mapping, validation, and scheduling
mechanisms in Rust.

### WP5.3 — MOS CLI and configuration

Move command interpretation, configuration policy, startup language selection,
and command registration to BASIC64 modules. Rust retains only storage and host
mechanisms exposed through capabilities.

### WP5.4 — FileSwitch and filing systems

Implement public file semantics, path policy, file handles, and FileSwitch
routing in BASIC64. Keep host filesystem access and checked bulk I/O as protected
primitives.

Preserve caller provenance for retained buffers and asynchronous operations.

### WP5.5 — VDU, graphics, fonts, and ColourTrans

Move public VDU parsing and graphics/colour/font policy into modules where
reasonable. Retain bounded raster access, host rendering, font engine calls, and
GPU submission as Rust primitives.

The work package must explicitly document any hot path retained as a primitive
and why it is mechanism rather than public policy.

### WP5.6 — Wimp and desktop service boundary

Move Wimp public dispatch and policy behind a BASIC64 module. Rust retains host
window/event/rendering mechanisms and isolation. Preserve task-local pointers,
poll behaviour, window ownership, and redraw contracts.

### WP5.7 — Project-specific services

Migrate or replace `Acorn_*` named services under Trellis-owned modules. Decide
which remain additive public SWIs and which should become higher-level module
APIs.

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

1. Start the existing Trellis environment normally.
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
