# Acorn-2026: Design Brief

> **Build the computer Acorn might have built in 2026.**

This is a design brief for a new, tinkerable computer environment inspired by Acorn and RISC OS. It is a re-imagining, not a RISC OS 3.71 simulator or a cosmetic remake. RISC OS supplies ideas and a valuable body of API knowledge; it does not dictate the implementation.

The architecture below records the current direction and identifies decisions that still need design work. It is not a claim that every detail is settled.

## 1. Project philosophy

The computer should be understandable from the first `PRINT` statement down to files, graphics, desktop services, memory, and machine code. Programming belongs inside the environment rather than behind a separate, heavyweight toolchain. A user should be able to write a small program, inspect how the desktop works, and modify system components with the same language and tools.

The guiding question for each inherited feature is:

> Does this preserve a useful Acorn idea, or merely an old implementation detail?

Preserve directness, inspectability, stable interfaces, fast startup, applications as tangible objects, a capable file manager, contextual interaction, drag-and-drop, and the sense that the computer is open to its owner. Let go of old hardware limits, pixel-era rendering assumptions, and compatibility burdens that do not serve those ideas.

The environment should feel descended from Acorn's design culture, not look like a high-resolution copy of an old desktop. It should be crisp, restrained, content-focused, and modern.

## 2. Scope and non-goals

### In scope

- A Rust runtime that supplies low-level, kernel-like services while running on a host operating system.
- BASIC64 as the built-in programming language, REPL, application language, and implementation language for much of the desktop and OS policy.
- A stable, globally available SWI service namespace, with `SYS` and module concepts retained.
- Per-task logical address spaces implemented by the runtime, with explicit module, shared, and system memory.
- Compatibility personalities for historical 32-bit BASIC behavior and native 64-bit BASIC64 programs.
- A Wimp-like desktop, Filer, application model, messaging, and modules, with most policy and user-facing behavior in BASIC64.
- System-owned graphics and typography using modern host rendering facilities.
- A compatibility execution or translation path for legacy ARM assembler where it is needed.

### Not the initial goal

- Building a bare-metal operating-system kernel, device-driver stack, or a new physical computer. The host OS remains responsible for hardware, process hosting, and drivers in the initial system.
- Recreating RISC OS 3.71 instruction for instruction or reproducing its appearance.
- Preserving 26-bit addressing, an application-slot memory limit, a single flat application/system address space, old display hardware, or other limits solely for nostalgia.
- Promising automatic, lossless recompilation of arbitrary RISC OS binaries. Binary archaeology and source reconstruction may be useful later, but are separate from the core design.
- Making every desktop component Rust code. Rust is for the trusted low-level core; BASIC64 is deliberately used above that boundary.

## 3. Architectural shape

The Rust runtime is the machine's low-level service core, not a traditional kernel. It mediates tasks, memory, service calls, input/output, and rendering through the host OS and appropriate host libraries. The BASIC64 runtime initially interprets programs; a JIT can be added after semantics and interfaces are stable.

```text
┌───────────────────────────────────────────────────────────────┐
│ User programs and applications              BASIC64           │
├───────────────────────────────────────────────────────────────┤
│ Desktop, Filer, Wimp-like policy, system tools BASIC64         │
├───────────────────────────────────────────────────────────────┤
│ BASIC64 runtime in Rust: parser, interpreter, memory, REPL     │
│                                      JIT later                 │
├───────────────────────────────────────────────────────────────┤
│ Global SWI dispatcher and module namespace                     │
│ Rust services             BASIC64 modules                      │
├───────────────────────────────────────────────────────────────┤
│ Rust runtime core: tasks, logical memory, IPC, host I/O,        │
│ graphics/text service boundary, scheduling policy              │
├───────────────────────────────────────────────────────────────┤
│ Host operating system, rendering libraries, hardware drivers    │
└───────────────────────────────────────────────────────────────┘
```

Rust owns mechanisms that need a small, dependable implementation boundary: task identity and scheduling, address translation, memory allocation and protection, service dispatch, inter-task communication, and access to host facilities. BASIC64 owns most system policy and user-facing behavior: desktop rules, Filer behavior, application conventions, and higher-level services. A service can be implemented in either language and still appear through the same SWI namespace.

This boundary is a starting point, not a demand to put every policy decision in one layer. Keep the trusted Rust core small enough to inspect, and make system behavior that users may reasonably want to change available as BASIC64 source.

## 4. BASIC64 and source compatibility

BASIC64 should remain recognizably BBC BASIC: immediate use, short programs, `PRINT`, `INPUT`, `FOR`, `REPEAT`, `PROC`/`FN`, `LOCAL`, `DIM`, hexadecimal constants, built-in graphics, and a path down to memory and assembly. It is a language for using and understanding the computer, not only for teaching programming.

The compatibility target is **BBC BASIC V/VI source semantics where feasible**. Existing documented source behavior should remain intact in a compatibility personality: syntax, operators and precedence, numeric and string behavior, control flow, error handling, built-in procedures/functions, memory operators, and interactions with `SYS` should be inventoried and treated as a compatibility contract. New BASIC64 capabilities should be additive or opt-in so they do not silently reinterpret established source.

“100% compatible” needs a bounded definition. Source-level compatibility does not itself promise that arbitrary ARM machine code, undocumented interpreter quirks, or hardware-specific code will run unchanged. The exact BASIC V/VI baseline, edge cases, and compatibility boundary are open design questions.

The runtime is written in Rust. Start with an interpreter because it makes the language semantics, memory model, and debugger approachable. Add a JIT later without changing program-visible behavior. Any future compilation strategy should target a stable internal representation and keep the REPL and interpreted path useful.

### Possible additive language evolution

The earlier language discussion explored features that could make BASIC64 feel like a modern continuation of BBC BASIC. These are options for Codex to evaluate, not a settled feature list. They must not silently change V/VI source semantics:

- Modern 64-bit types, records/value types, collections, and clearer procedure/function signatures.
- Lexical scope and explicit global declarations in new code, while retaining classic `LOCAL` and variable behavior in compatibility code.
- Modules and imports, structured error handling alongside `ON ERROR`, and Unicode strings with useful interpolation and slicing.
- Direct iteration and ranges, with concurrency or asynchronous tasks available for programs that need them.
- Built-in graphics and approachable GUI creation, so the path from a short program to a useful application remains short.
- Low-level memory access and an integrated assembler, mediated by logical addresses and an explicit execution target.

Keep the language concise and direct. Avoid making BASIC64 a syntax-heavy systems language or requiring a large framework for ordinary programs.

## 5. Services, SWIs, and modules

### Keep the service contract stable

API compatibility is a central constraint. Preserve familiar service names, `SYS` usage, module discovery/registration concepts, argument and result conventions, error behavior, and documented SWI semantics wherever feasible. Change an externally visible contract only for a clear reason, document the incompatibility, and provide an adapter or compatibility route where practical.

Do not confuse the historical implementation with the public contract. A SWI can keep its name and calling behavior while its handler uses Rust-owned task contexts and translated memory access rather than dereferencing a process-wide host pointer. BASIC64 may add higher-level APIs, but those should sit alongside the stable service surface.

### One namespace, multiple implementation languages

The dispatcher exposes one global SWI namespace. A provider may be written in Rust or BASIC64. The caller should not need to know which language implements a service.

```text
                    Global SWI namespace
                    /                   \
          Rust service               BASIC64 module
                 \                    /
                  SWI caller context
                           │
                 translated memory access
```

The Rust core provides foundational services and the execution/runtime machinery. The desktop, Filer, Wimp-like behavior, and most higher-level OS policy can be BASIC64 modules. Module workspaces belong to modules; parameters supplied by an application belong to the calling task. The dispatcher carries caller identity across the whole call chain.

`SYS`, module concepts, and the RISC OS service vocabulary are valuable points of continuity. They should remain usable even though the implementation beneath them is new.

## 6. Address spaces and memory

### Addresses are local; services are global

Each task gets a logical address space managed by the Rust runtime. An address is an offset in a particular task's logical machine, not a Rust pointer and not necessarily a host virtual address. Two tasks may use the same numeric address for different memory.

```text
Task A logical space                 Task B logical space
┌─────────────────────┐              ┌─────────────────────┐
│ program and globals │              │ program and globals │
│ heap / DIM blocks   │              │ heap / DIM blocks   │
│ stack / workspace  │              │ stack / workspace  │
│ same guest address │              │ same guest address │
│ can mean other data│              │                     │
└─────────────────────┘              └─────────────────────┘
          │                                      │
          └──────── explicit shared mappings ───┘

        Global services: SWIs, modules, Rust runtime
```

The interpreter already mediates BASIC memory operations, so a software-MMU-style abstraction can translate and validate addresses without requiring a physical MMU. A later JIT must preserve that abstraction; it must not turn logical addresses into unchecked host pointers.

### Memory classes

| Class | Owner and visibility | Typical use |
|---|---|---|
| Task | One task | Program, heap, stack, `DIM`, private data |
| Module | A loaded module and its calls | Module code, workspace, persistent service state |
| Shared | Explicitly granted to participating tasks | Shared buffers, inter-task data, evolved Dynamic Areas |
| System | Rust runtime and trusted system services | Dispatcher, runtime metadata, protected core state |

The namespace and services are global; application memory is task-local. Sharing is explicit. A task may corrupt its own writable memory as a normal low-level program can, but it cannot use an arbitrary logical address to overwrite another task, a module, or the runtime.

### SWI caller context and pointer translation

Every service call carries a caller context: task identity, execution personality, and address-space identity. A legacy SWI argument that represents a pointer is resolved in the caller's space. A service receives a checked memory view or translated buffer access, not an unqualified host pointer.

```text
Application Task 17
    SYS "OS_File", ..., filename%
             │
             ▼
SWI dispatcher: caller = Task 17
             │
             ├── scalar arguments remain values
             └── pointer arguments resolve in Task 17 space
                         │
                         ▼
              Rust or BASIC64 service
```

For synchronous calls, the pointer is normally read or written during the call. If a module retains a pointer, uses it after return, invokes a callback later, or shares a buffer with another task, the runtime must retain its provenance and lifetime. Internally that reference needs at least an address-space identity plus an offset, with access rights and a lifetime/ownership rule. The service's externally visible legacy call can remain unchanged while the runtime stores a richer reference.

SWI metadata or service descriptors can identify scalar, input buffer, output buffer, in/out buffer, and retained-reference arguments. This enables translation and validation without changing the public API. Long-lived pointers, vectors, callbacks, and shared structures need explicit per-service rules; they cannot safely be inferred from a numeric address alone.

### 32-bit compatibility and native 64-bit personalities

Do not force old code to adopt 64-bit integer or address semantics. Provide two logical execution personalities:

| Personality | Program-visible model |
|---|---|
| BBC BASIC V/VI compatibility | 32-bit logical machine, historical numeric and pointer assumptions, familiar memory layout and SWI behavior |
| Native BASIC64 | 64-bit logical address space and native-width types, with the same global service namespace |

Both can call a service such as `OS_File`; the service operates on caller context rather than assuming that every task has the same layout. The exact address ranges, integer suffixes, narrowing rules, and selection of a personality remain open.

### Dynamic Areas

Retain the Dynamic Area concept and compatible SWIs where practical, but allow the implementation to evolve into runtime-managed regions that can be mapped into one or more tasks. A legacy view can preserve familiar call behavior. A newer shared-memory use can provide explicit naming/handles, mapping rights, resize rules, and lifetime management. It should not require every application to share a single global address space.

## 7. ARM assembler and legacy execution

BBC BASIC source can contain assembler that assumes its registers contain directly dereferenceable ARM addresses. That code cannot safely run as native host code when BASIC addresses are logical offsets. Keep this compatibility problem behind an ARM execution or translation layer:

```text
Legacy BASIC assembler source
           │
           ▼
ARM32 instruction stream
           │
interpreter / compatibility executor / translator
           │
logical task memory and SWI bridge
```

An initial implementation may interpret the supported ARM subset. Translation can follow if performance requires it. Memory loads/stores and service calls must go through the task's logical memory and caller context. New native BASIC64 assembly can be designed separately and must declare its target and access model; it must not redefine the legacy assembler's meaning.

The needed ARM versions and edge cases (including 26-bit-era behavior) should be scoped from real compatibility needs. Preserving BASIC assembler source semantics does not imply emulating every historical processor behavior or running every binary.

## 8. Graphics and system-owned typography

The Rust runtime should expose a modern drawing/composition service and use capable host rendering libraries rather than treating the display as a pixel array. The concrete library and host backends are implementation choices for a later prototype. BASIC graphics, Wimp-like controls, Filer content, and applications should converge on the same service.

Text is a first-class graphics primitive alongside paths, images, surfaces, transforms, clipping, and paint. The system owns text shaping and measurement so applications do not each choose a separate text stack.

```text
Text and font request
        ↓
Unicode/script segmentation and bidirectional analysis
        ↓
font fallback and OpenType shaping (HarfBuzz-level capability)
        ↓
glyph positions and layout metrics
        ↓
rasterization and composition through host rendering facilities
```

Modern shaping should support complex scripts, combining marks, ligatures, bidirectional text, font fallback, variable fonts, and modern Unicode. Applications ask the system to measure, lay out, and draw text; they do not need to know shaping-engine internals.

Preserve the Font Manager concept and familiar SWIs such as `Font_FindFont`, `Font_Paint`, and `Font_StringWidth`. Their names and calling contracts remain stable where feasible. Modern implementations can provide better shaping beneath them.

Compatibility belongs in profiles, not a normal global “HarfBuzz on/off” switch. Legacy applications can receive text metrics and behavior appropriate to their compatibility profile; new BASIC64 applications and the desktop use modern shaping by default. A developer-only setting may force profiles for diagnosis. Measurement, wrapping, caret placement, hit testing, menu sizing, and painting must agree within each profile.

## 9. Desktop, Filer, and application model

The desktop is a BASIC64 system made from inspectable, modifiable components. The Filer is a central part of the environment; applications communicate through defined messages and shared services instead of relying on deep coupling. A Wimp-like event and window model can provide the familiar conceptual interface while the Rust core handles low-level window, input, and graphics mechanisms.

Preserve and evolve these ideas:

- A compact application-centered desktop and a refined icon bar rather than a permanent layer of toolbars.
- Three-button roles—Select, Menu, and Adjust—where hardware supports them, with accessible mappings on devices that do not.
- Contextual menus and direct manipulation.
- Drag-and-drop as a protocol, especially drag-to-save and drag-to-open.
- File types as first-class metadata, with applications declaring which types they understand.
- Applications as tangible objects that can be inspected, copied, moved, and removed.
- A message-based application model with explicit open, load, save, and data-transfer interactions.

An application bundle should be an inspectable directory, not an opaque binary container. It can hold BASIC64 source, metadata, icons, resources, and an entry point. Copying a bundle should be an understandable way to move an application. The runtime may provide indexing, previews, and search over file metadata without replacing the simple visible file model.

Tinkerability is a design requirement: desktop components should be readable and modifiable in BASIC64, the system should expose useful examples and tools, and there should be a straightforward route from editing a component to trying it. Keep safeguards and trust boundaries in the runtime, but do not bury ordinary system behavior in inaccessible machinery.

## 10. What to preserve and what to leave behind

| Preserve or evolve | Do not preserve solely for nostalgia |
|---|---|
| BBC BASIC's immediate, direct feel | 26-bit addressing and old processor status tricks |
| Stable SWI and module concepts | A single flat address space for all applications and services |
| Filer, typed files, application bundles | Application-slot and historical memory limits |
| Three-button interaction roles | 16-color sprite/display assumptions |
| Wimp-style messaging and contextual UI | Pixel plotting as the only drawing model |
| Drag-to-save and tangible applications | Old screen geometry and exact desktop appearance |
| Built-in graphics and access to low-level programming | Native execution of old ARM code without a compatibility boundary |
| User inspection and modification | A global shaping toggle that silently changes old layouts |

The rule is: **preserve the contract, not obsolete implementation**. Where compatibility itself is the contract, keep it. Where an old mechanism was only one way to deliver the idea, replace it.

## 11. Suggested phased roadmap

### Phase 0 — Write down the contracts

Inventory the BASIC V/VI source surface and documented SWIs/modules. Mark each behavior as required compatibility, an intentional extension, or out of scope. Define the minimum Rust/BASIC64 boundary and the meaning of each memory class.

**Exit:** a reviewable compatibility matrix and service catalog exist before implementation choices harden.

### Phase 1 — Hosted Rust runtime and BASIC64 interpreter

Build a hosted runtime with task identity, a BASIC64 REPL, source loading, and an interpreter. Start with the chosen compatibility subset and make the logical-memory interface part of the interpreter rather than a later retrofit.

**Exit:** BASIC programs can run interactively and in tasks without exposing host pointers as BASIC addresses.

### Phase 2 — SWI dispatch and logical memory

Implement the global service dispatcher, caller contexts, task/module/shared/system memory classes, checked pointer translation, and a small foundational service set. Provide descriptors for pointer-bearing calls and define retained-reference behavior.

**Exit:** a service can be implemented in Rust or BASIC64 and receive the same caller-aware service context.

### Phase 3 — BASIC64 modules and desktop foundation

Load BASIC64 modules through the shared namespace. Establish messaging, windows, input, the Filer, file-type registration, and inspectable application directories. Put higher-level policy in BASIC64.

**Exit:** a BASIC64 desktop component can be inspected, modified, loaded, and communicate with an application through published services.

### Phase 4 — Graphics and typography

Connect a host rendering backend to shared graphics primitives. Add system text shaping, font fallback, measurement/layout, modern text, and a legacy compatibility profile. Keep text measurement and drawing on one coherent path.

**Exit:** the desktop and BASIC64 programs use the same service for text and graphics, with profile behavior selectable per application.

### Phase 5 — Compatibility depth

Expand BBC BASIC V/VI source coverage and the stable SWI surface according to the compatibility matrix. Add legacy Dynamic Area behavior and, if justified by target programs, the ARM execution/translation layer for assembler.

**Exit:** compatibility is measured against explicit source and service requirements, with known unsupported cases documented.

### Phase 6 — JIT and performance

Add a JIT behind the interpreter's established semantics and logical-memory interfaces. Preserve the interpreter as a debugging and reference path. Optimize only after representative BASIC64 and desktop workloads are known.

**Exit:** interpreted and JIT execution share observable behavior for the supported language and service contracts.

### Phase 7 — Portability and broader hardware ambition

Evaluate additional host systems, rendering backends, packaging, and possible dedicated hardware only after the hosted environment demonstrates the design. Bare-metal work is a separate architectural step, not a prerequisite for the Acorn-2026 experience.

**Exit:** platform-specific code remains behind stable runtime and rendering boundaries.

## 12. Open design questions for Codex

Work through these in order; preserve open questions rather than silently converting guesses into requirements.

1. **Compatibility baseline:** Which exact BBC BASIC V and VI versions, documented behaviors, extensions, and known quirks define source compatibility? Which programs form the representative compatibility set?
2. **Compatibility selection:** How does a program select the 32-bit compatibility personality—source metadata, application manifest, launcher setting, or another mechanism? What should happen when no personality is declared?
3. **BASIC64 evolution:** Which new language features are essential at the start? How do integer suffixes, pointer/address types, overflow, string representation, and new syntax coexist with historical semantics?
4. **SWI contract:** Which parts of the register/calling convention, errors, flag behavior, argument blocks, and module lifecycle must remain byte-for-byte compatible? How are extensions versioned without changing existing calls?
5. **Pointer descriptors:** How are pointer-bearing SWI arguments described? What are the exact rules for buffers retained after return, callbacks, vectors, async I/O, and module-held references?
6. **Shared memory and Dynamic Areas:** What names or handles identify a shared region? Who can map it, resize it, revoke it, or free it? How are old Dynamic Area calls represented?
7. **Task and module lifetime:** Are modules reentrant? How is module workspace allocated per call or per task? What happens to retained references when a task or module exits?
8. **Address layout:** What logical ranges and alignment rules are visible in each personality? Which historical BASIC memory variables/operators must retain exact meaning?
9. **ARM compatibility scope:** Which ARM ISA generations and legacy modes are required? Is an interpreter sufficient initially, and what semantics must a translator preserve for memory and SWIs?
10. **Rendering backend:** Which host library best fits the first prototype's platforms, 2D drawing, text, and compositing needs? How will rendering and windowing remain replaceable?
11. **Text profiles:** Which legacy metrics and layout behaviors can be reproduced? How does an application declare a profile, and how are profile changes prevented from breaking measurement/painting consistency?
12. **Desktop input:** How should Select/Menu/Adjust work on a trackpad or two-button mouse, and what accessibility remapping is needed?
13. **Application bundles:** What is the directory layout and manifest format? How are file types, launch behavior, permissions, updates, and resource lookup described while keeping the bundle inspectable?
14. **Trust boundary:** Which services are safe for every task, which require capabilities, and which components are trusted? How does BASIC64 system code receive privileges without giving every application access to system memory?
15. **First host target:** Which host OS and runtime architecture should the first useful prototype support? What is the smallest end-to-end demonstration that proves the design: REPL, file handling, graphics, a BASIC64 desktop component, or all four?

## 13. Working principles

- **Build the computer Acorn might have built in 2026.**
- **Preserve the contract, not obsolete implementation.**
- **Addresses are local. Services are global.**
- Keep documented compatibility stable; require a very good reason to change a public API.
- Keep the trusted Rust core focused on mechanisms; implement most user-facing OS policy in inspectable BASIC64.
- Make modern capabilities system-wide services so every application can benefit without selecting its own stack.
- Keep old and new execution personalities explicit when their semantics differ.
- Treat implementation details and unresolved choices as questions until evidence settles them.
