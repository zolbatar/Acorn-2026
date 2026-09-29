# Trellis: Live System Architecture

## Status

This document records the architectural direction for **Trellis**, the project
previously described under the working name Acorn-2026. It is the basis for
future work packages.

The mission, module ownership of SWIs, absence of native public SWI handlers,
and bootstrap boundary described here are firm design decisions. The complete
live-system experience, language extensions, persistence model, optimisation
strategy, and desktop projections remain staged design work.

## Mission

> **Trellis makes the computer understandable, programmable and malleable by
> the person using it—continuing the Acorn tradition through a modern, live and
> inspectable RISC OS environment.**

## Architectural charter

> **Trellis is a RISC OS-compatible hosted environment in which BASIC64
> definitions, system services, tasks and resources have stable logical
> identities and inspectable relationships. Selected BASIC64 behaviour can be
> replaced live through versioned definitions, while Rust enforces isolation,
> capabilities and machine-facing mechanisms.**

Trellis is a new live operating environment with a deliberately compatible
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
may add facilities required to build Trellis, such as modules, closures,
structured values, exceptions, tasks, reflection, and controlled dynamic
dispatch. Extensions must be additive or explicitly selected; they must not
silently reinterpret compatible BBC BASIC source.

The language should grow from concrete requirements imposed by the next system
layer. Trellis does not assume that every operation requires universal
Smalltalk-style late binding. Static or guarded calls are appropriate where
they preserve live replacement and inspectability.

The interpreter remains the reference and universal execution path. Portable
IR can feed an interpreter, a tiered JIT, and later AOT compilation. Optimised
code must preserve caller identity, logical memory checks, capabilities, source
locations, dependency information, invalidation, and deoptimisation.

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

> **Every Trellis SWI is exported by a module and implemented by a versioned
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

A Trellis module is a live BASIC64 package that may contain:

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

### Live replacement

New calls enter the current definition generation. Calls already executing may
finish against the retired generation. Retired code and state remain alive
until no active invocation or retained reference requires them.

Replacement falls into explicit classes:

- immediate replacement for stateless compatible definitions;
- quiescent replacement after active calls finish;
- migrating replacement with a checked state-upgrade definition;
- restart-required replacement when foundational invariants change.

Replacement is rejected if validation, contract checking, capability checks,
or state migration fail. Live modification is not permission to corrupt the
system silently.

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

The capsule contains visible BASIC64 source or rebuildable portable IR, module
manifests, dependencies, primitive imports, SWI exports, integrity metadata,
and optionally discardable native-code caches.

The initial capsule contains small foundation modules such as:

```text
System
ModuleManager
Error
TaskManager
Memory
Console
Boot
```

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

The boot capsule should contain enough to reach a usable recovery command line.
It need not contain every normal desktop component.

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

- the exact BASIC64 module and manifest syntax;
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
