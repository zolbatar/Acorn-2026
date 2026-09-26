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
- Promising automatic, lossless recompilation of arbitrary RISC OS binaries. The planned JIT/AOT path does include eligible BBC BASIC I–VI source and tokenised programs under their respective compatibility semantics; translating arbitrary pre-existing machine-code binaries remains separate work.
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

BASIC64 is the system's native language; earlier BASIC syntax defines compatibility paths, not the language used to implement the system by default. A firm requirement is that the system can load programs from all earlier BBC BASIC versions. The user has described the tokenized saved-program format as shared. Real BBC Micro and BBC Master files show one shared-boundary layout; the ARM BASIC V fixture uses a separate line terminator and record marker. One decoder accepts both layouts and preserves token bytes. The Master TETRIZ 1.5 fixture adds BASIC IV-class evidence, though its exact ROM revision is unknown and it does not cover every BASIC IV-specific token. Shared-boundary execution begins with the deliberately narrow leading-`REM`, literal-string `PRINT`, and `END` common-token subset; the detected record layout does not identify an exact BASIC release, and unsupported tokens fail explicitly. Version-specific token interpretation and execution remain separate compatibility work. The current evidence and gaps are tracked in [`docs/tokenized-basic-compatibility.md`](tokenized-basic-compatibility.md). The native BASIC64 source format starts as plain UTF-8 text with a `.bas64` extension and a separate pest grammar. The first encoded compatibility fixture is ClockSP5 program version 5.08, kept beside its text source.

“100% compatible” needs a bounded definition. Source-level compatibility does not itself promise that arbitrary ARM machine code, undocumented interpreter quirks, or hardware-specific code will run unchanged. The exact BASIC V/VI baseline, edge cases, and compatibility boundary are open design questions.

The runtime is written in Rust. Start with an interpreter because it makes the language semantics, memory model, and debugger approachable. Add a JIT later without changing program-visible behavior. Any future compilation strategy should target a stable internal representation and keep the REPL and interpreted path useful.

### Clean BASIC64 and optimization eligibility

Consider a future “clean” classification for BASIC64 code that the compiler can analyze and optimize with fewer unknown effects. Clean code would use managed BASIC values and memory, avoid arbitrary address-based reads and writes, and not modify its own executable code. Other checks may exclude inline assembler, dynamic code generation, or calls whose memory and control-flow effects cannot be described. Ordinary variables, arrays, procedures, and services with known contracts can remain eligible.

Treat clean as a property established by analysis or validation, not merely a promise in a source annotation. The useful unit—procedure, module, or whole program—and the handling of dynamic calls and dependencies remain open. An unknown or disqualifying operation can keep the affected code interpreted, or require an explicit runtime boundary; it must not silently make unsafe assumptions in compiled code. A clean classification indicates optimization eligibility, not trust or isolation: logical addresses, SWI caller context, and runtime protection rules still apply.

This could give the JIT a conservative path first and make ahead-of-time or native compilation practical for a sufficiently analyzable subset later. It is especially relevant to the WIMP, Filer, and other OS components intended to be written in BASIC64: those components could stay inspectable and modifiable while their stable, clean portions receive acceleration. Any compiled path must preserve the same observable BASIC and service behavior as interpretation.

### Compilation units and execution paths

Keep BASIC64 source as the editable source of truth, and compile from a stable internal representation rather than directly from syntax to host instructions. A bytecode or other portable IR interpreter can remain the universal path and reference implementation. The same representation can feed a JIT and, later, an ahead-of-time (AOT) compiler; source locations and runtime checks should survive lowering for debugging, errors, interruption, and service calls.

Separate the unit that is compiled from the unit used to package or cache it. A procedure or function is a useful first native compilation unit: cold or ineligible routines can stay interpreted while clean hot routines are compiled incrementally. A source file, BASIC64 module, or application bundle can be the build/cache unit, containing compiled routines plus dependency metadata. Keep `.bas64` source visible and editable; any native output should be a derived, target-specific artifact keyed by source and dependency hashes plus the compiler/runtime ABI, and should be rebuildable when stale. A bundle suits a closed application with resources and dependencies; a single source file is convenient when it is self-contained. Neither choice requires every routine in that file or bundle to compile.

A practical progression is to establish one IR and interpreted behavior first, then add a hybrid JIT that compiles eligible procedures on demand or after they become hot, falling back to interpretation for unsupported operations. Consider AOT later for stable BASIC64 modules and bundled system components, where known dependencies can be built in advance and startup or repeated JIT cost matters. Keep calls to dynamic services and unresolved imports behind runtime dispatch unless the compiler can prove their targets stable. Native code must still execute as the task, use the caller-aware SWI boundary, and access guest memory through the runtime; native compilation does not grant host pointers or extra privileges.

Cranelift is the preferred initial compiler backend because its Rust-embeddable integration is lighter. Keep the BASIC64 IR and compiler boundary backend-neutral, and validate Cranelift's compile latency, generated-code quality, target coverage, artifact support, and maintenance cost against real workloads. Reconsider [LLVM ORC](https://llvm.org/docs/ORCv2.html) only if those measurements or a required feature justify its larger integration.

### Classic BASIC compatibility and compiled execution

The eventual JIT/AOT path is not limited to BASIC64. It should compile eligible programs for every classic BBC BASIC compatibility personality the runtime supports, with BBC BASIC I–VI as the long-term coverage goal. Classic source and decoded tokenised programs should lower through their selected version profile, not be silently reinterpreted as BASIC64. A tokenised file's record layout alone does not establish its language version; the selected compatibility profile and its semantic version must be part of compilation and cache identity.

For each implemented classic profile, compiled execution must preserve the interpreter's observable numeric model, operators, control flow, variables and arrays, built-in behavior, errors, memory operations, and SWI effects. Operations with understood semantics can lower to the shared IR or call checked runtime helpers. Unsupported or opaque operations can remain interpreted or use a target-specific compatibility executor while eligible procedures compile, so one such operation need not disqualify an entire program. Embedded ARM or 6502 machine code remains a distinct execution target; compiling BASIC source does not translate arbitrary machine-code routines into host instructions. Compiled artifacts should retain source or tokenized-line locations and record the compatibility profile, runtime ABI, dependencies, and host target used to build them.

### BBC BASIC V procedure libraries

BBC BASIC V's `LIBRARY` and `INSTALL` commands load separate saved BASIC programs that normally contain `PROC` and `FN` definitions. RISC OS BASIC V also has `OVERLAY` libraries, loaded on demand when a named procedure or function is called. Procedure lookup searches the main program first, then `LIBRARY` files (most recently loaded first), `INSTALL` files (in reverse load order), and finally the overlay list. These are name-searched procedure libraries rather than isolated modules with a declared ABI: resolution depends on the active library set, and library code may rely on the caller's BASIC variables and runtime state. The guide also warns that line-number references in a library refer to the main program, and recommends keeping library routines self-contained. Preserve these loading and lookup rules in the compatibility personality. They can still be compilation inputs: analyze and compile individual resolved definitions when eligible, while retaining runtime name resolution and invalidating compiled entries when the active definition or its dependencies change. Do not treat a library file's boundary as proof that all its routines are clean or statically closed. The [BASIC V procedures and libraries guide](https://www.riscos.com/support/developers/basicv/chap05.htm) describes the library model and its `LIBRARY`, `INSTALL`, and `OVERLAY` behavior.

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

The Rust runtime should expose a modern drawing/composition service. The first hosted display uses one `winit` window and a `pixels` framebuffer so the MOS/BASIC text display and graphics share the same visible surface. This is the initial compatibility renderer; it does not constrain the later desktop composition service. BASIC graphics, Wimp-like controls, Filer content, and applications should converge on the same service.

Window keyboard bytes feed the hosted console input source. The runtime reads them on its own thread through `OS_ReadC` and `OS_ReadLine`, while display events update the window's graphics scene. This allows a synchronous BASIC `INPUT` statement to wait for a line without stopping window event processing; visible text continues to use the normal SWI output path.

### First compatibility graphics slice

The hosted compatibility path begins with a stateful VDU stream at `OS_WriteC` and the standard `OS_Plot` SWI (`&45`). `OS_WriteS`, `OS_Write0`, and `OS_NewLine` continue to reach the same byte stream through `OS_WriteC`. The stream parser retains partial VDU commands across calls, so the command byte and its parameters may arrive separately. The contracts follow the [RISC OS VDU driver](https://www.riscos.com/support/developers/prm/vdu.html) and [VDU code table](https://www.riscos.com/support/developers/prm/vducodes.html).

The initial BASIC V/VI surface adds `MODE`, `VDU`, `LINE`, `MOVE`, `DRAW`, `PLOT`, `GCOL`, and `PRINT TAB(x,y)`. `MODE` and `VDU` send their control bytes through `OS_WriteC`; `LINE` and the related plot statements use `OS_Plot`. The hosted profile models BBC modes 0–7, logical coordinates, text and graphics windows, graphics origin and cursor, basic colour state, a text-cell surface, and retained line/point primitives. It handles VDU 12, 17, 18, 22, 24, 25, 26, 28, 29, 30, and 31, plus common text cursor controls. A selected 32-bit extended mode descriptor is also supported with its pixel dimensions and X/Y eigenfactors. The BASIC syntax follows the [simple graphics](https://www.riscos.com/support/developers/bbcbasic/part2/simplegraphics.html), [complex graphics](https://www.riscos.com/support/developers/bbcbasic/part2/complexgraphics.html), and [VDU control](https://www.riscos.com/support/developers/bbcbasic/part2/vducontrol.html) chapters. Other VDU commands are not claimed as implemented merely because the stream parser consumes their documented parameter count.

The standard screen modes render into their mode-sized pixel buffers. Extended 32-bit mode uses a shared RGBA raster surface: plot calls update the surface directly, so a high-volume program does not retain one scene primitive per plotted pixel. Display snapshots share that surface and continue to publish at most about 60 times per second during `BASICRUN`. This path supports the selected C16M descriptor and the `ColourTrans_ConvertHSVToRGB` and `ColourTrans_SetGCOL` calls used by the full Mandelbrot listing; it is not a general RISC OS mode-selection or ColourTrans implementation. `cargo run` opens the window; `cargo run -- --stdio` keeps the terminal host available for command-line use. Legacy saved-file decoding remains shared, but execution semantics are selected by the compatibility profile. Graphics support does not imply machine-code execution. `CALL` needs a separately selected processor compatibility service; no 6502 emulator is introduced by this graphics work.

The first end-to-end display milestone accepts `HELP` and `QUIT` at the in-window MOS prompt and runs [`examples/graphics/text-and-pixels.bas`](../examples/graphics/text-and-pixels.bas) from its tokenised companion file. The program prints text and plots points on the same framebuffer. This is an initial compatibility display, not the completed desktop graphics service.

The authentic TDU-01 file remains the working source for the common graphics slice and useful evidence for a real BBC Micro saved-program layout. Develop the language-level `MODE`, VDU, and drawing behavior it contains through the standard SWI services. TDU-01 itself is not a required release-compatibility or whole-program acceptance target: its procedure flow, direct memory operations, and machine-code routines extend beyond this slice. The source-derived Mandelbrot fixture exercises its selected extended C16M path through generated ARM BASIC V tokens; this is a program-specific integration case, not broad BASIC V/VI or RISC OS compatibility. The compatibility corpus records these distinctions in [`docs/tokenized-basic-compatibility.md`](tokenized-basic-compatibility.md).

### Font sources and rendering

The BBC Micro compatibility display uses the supplied 8x8 MOS character bitmap for character codes 32–127. Preserve these glyph bytes and draw them directly into the shared framebuffer for classic BBC text modes; this bitmap is distinct from the RISC OS desktop fonts. Codes below 32 remain VDU control characters, and handling of additional character sets can be added when a real program requires them. The table is available to the renderer in [`src/font.rs`](../src/font.rs).

RISC OS system faces such as Corpus, Homerton, and Trinity use their native Font Manager resources as the authoritative assets: `IntMetrics`, `Outlines`, encoding data, and any supplied size-specific bitmap files. Read the native metrics and outlines, rasterize glyphs for the requested size, and cache the resulting bitmaps for drawing. This keeps the Acorn outlines and spacing while avoiding outline rasterization on every frame. Do not make TrueType/OpenType conversion a required asset-preparation step; generated derivatives may be used for interoperability or comparison, but are not the source of truth. The native RISC OS font files have not yet been added to this checkout.

The two paths remain selectable by rendering profile: BBC compatibility text uses the fixed 8x8 bitmap; RISC OS system text uses Font Manager metrics and scalable outlines. Both are rendered into the same window and framebuffer as graphics.

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

### Guest paths and host paths

RISC OS-style paths use `.` between directory components, so guest paths must not be interpreted with the host's path rules. The current prototype's `RUN` and `BASICLOAD` commands accept host paths only as bootstrap conveniences; that syntax is not the guest path contract. Record the distinction now, and implement guest path parsing and host-filesystem mapping with the filesystem, `OS_File`, and Filer layer rather than expanding the current commands' host-path behavior. The exact root/current/parent notation, device syntax, file-type handling, and escaping rules remain to be specified there.

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

### Phase 0 — Define the first MOS prompt contract

Specify the first hosted prototype: a Rust command line that reaches the `*` prompt and accepts `HELP` and `QUIT`. Define the initial console SWI subset (`OS_WriteC`, `OS_WriteS`, `OS_Write0`, `OS_NewLine`, `OS_ReadC`, `OS_ReadLine`, and `OS_CLI`), their caller-memory rules, and the manual acceptance sequence. Defer BASIC64 and the broad compatibility inventory to later phases.

**Exit:** a prompt acceptance contract and initial SWI catalog exist before implementation choices harden.

### Phase 1 — Hosted Rust MOS prompt

Build a RustRover-ready Cargo project with one command task, a hosted console adapter, a caller-aware SWI dispatcher for the initial console/CLI subset, and the `HELP` and `QUIT` commands. Route prompt, line input, command dispatch, help, and errors through the corresponding SWIs. `QUIT` ends the hosted runtime cleanly. This is a bootstrap sequence; BASIC64 remains part of the long-term architecture and follows this milestone.

**Exit:** `cargo run` reaches `*`; entering `HELP` displays help through the SWI output path and returns to `*`; entering `QUIT` ends the runtime without exposing host pointers as guest addresses.

### Phase 2 — BASIC64 runtime and full logical-memory/SWI model

Build the BASIC64 interpreter around a separate pest grammar and start with runnable UTF-8 `.bas64` programs. The first executable slice supports `INPUT` and `PRINT` of string variables through the existing console SWIs. `BASICLOAD` decodes the observed shared-boundary and separate-terminator saved-program layouts, preserves token bytes, extracts line references, and retains a loaded program. The corpus and its version evidence are recorded in [`docs/tokenized-basic-compatibility.md`](tokenized-basic-compatibility.md); this decoder coverage does not yet establish every earlier BASIC release. `BASICRUN` executes the legacy echo fixture and a fixture-focused compatibility subset sufficient to run ClockSP5 program version 5.08 through its three benchmark passes and back to the MOS prompt. The hosted profile supplies monotonic centisecond `TIME` and nonblocking host-key `INKEY` (`-256` for bare `INKEY` and `-1` for `INKEY(0)` when no key is pending); ClockSP5's own guards skip native ARM and hardware setup, while its final hardware reset command is a no-op. This is a program-specific acceptance milestone, not general BASIC V/VI execution or a measurement of the host processor's physical clock. A source-derived Mandelbrot fixture separately exercises parameterized routines, indirect strings/words, the selected C16M extended mode block, and the two `ColourTrans` calls used by that listing; this remains sample-specific coverage, not a general BASIC V/VI or RISC OS profile. Continue with the REPL, full global service dispatcher, caller contexts, task/module/shared/system memory classes, checked pointer translation, and retained-reference rules. Build the version-aware compatibility execution path for all earlier BASIC versions. Make logical memory part of the interpreter from its start.

**Exit:** native BASIC64 programs can run interactively and call services through the same caller-aware context without exposing host pointers as BASIC addresses, and tokenised programs from all earlier BASIC versions can be loaded through the shared file-format decoder and a version-aware compatibility path.

### Phase 3 — BASIC64 modules and desktop foundation

Load BASIC64 modules through the shared namespace. Establish messaging, windows, input, the Filer, file-type registration, and inspectable application directories. Put higher-level policy in BASIC64.

**Exit:** a BASIC64 desktop component can be inspected, modified, loaded, and communicate with an application through published services.

### Phase 4 — Graphics and typography

The first compatibility renderer is in place: one `winit` window backed by `pixels` renders the MOS prompt, BASIC text, and pixel/line primitives together. Standard modes use their mode-sized framebuffer; the selected extended 32-bit C16M path draws into a shared RGBA surface and publishes snapshots at up to about 60 Hz, avoiding an unbounded retained plot list for per-pixel programs. This is a compatibility slice for the Mandelbrot listing, not general extended-mode or ColourTrans support. Next, improve palette and plot-action fidelity and broaden mode coverage. Add RISC OS native font loading, text shaping, font fallback, measurement/layout, and modern text. Keep text measurement and drawing on one coherent path.

The hosted window accepts system clipboard text through Command+V on macOS (Control+V on other hosts). Pasted text follows the same input channel as keystrokes: CR, LF, and CRLF line endings submit lines through `OS_ReadLine`; tabs become spaces, printable ASCII is retained, and unsupported characters are ignored. This keeps console input inside the existing SWI path.

**Exit:** the desktop and BASIC64 programs use the same service for text and graphics, with profile behavior selectable per application.

### Phase 5 — Compatibility depth

Expand BBC BASIC V/VI source coverage and the stable SWI surface according to the compatibility matrix. Add legacy Dynamic Area behavior and, if justified by target programs, the ARM execution/translation layer for assembler.

**Exit:** compatibility is measured against explicit source and service requirements, with known unsupported cases documented.

### Phase 6 — JIT and performance

Add a JIT behind the interpreter's established semantics and logical-memory interfaces. Preserve the interpreter as a debugging and reference path. Use representative BASIC64, classic BASIC compatibility, and desktop workloads to guide optimization, and evaluate clean-code analysis as a way to identify code suitable for JIT or, later, native compilation.

An isolated feasibility prototype parses a ClockSP5-derived ARM BASIC V fixture with the compatibility parser, lowers its integer assignments and nested `REPEAT`/`UNTIL` loops into a small backend-neutral typed IR, and maps that IR to Cranelift JIT and object code in `tools/basic-jit-bench`. The MOS prompt also has an opt-in `BASICJIT` hybrid experiment behind the `experimental-jit` Cargo feature. It recognizes and compiles the full Mandelbrot listing's outer raster loops and iteration math into one Cranelift frame kernel; ColourTrans and `OS_Plot` effects use checked runtime callbacks, while mode setup and the final key wait remain interpreted. The reduced Mandelbrot fixture uses its verified iteration kernel, and ClockSP5 uses its verified nested integer-repeat region. `BASICRUN` remains the interpreter reference path. This remains sample-specific work: it does not establish the production IR or runtime ABI, compile either demo generically, or settle AOT packaging.

**Exit:** interpreted and compiled execution share observable behavior for BASIC64 and each implemented classic compatibility profile; compiled artifacts can be tied to the source, compatibility profile, target, and runtime dependencies they were built from.

### Phase 7 — Portability and broader hardware ambition

Evaluate additional host systems, rendering backends, packaging, and possible dedicated hardware only after the hosted environment demonstrates the design. Bare-metal work is a separate architectural step, not a prerequisite for the Acorn-2026 experience.

**Exit:** platform-specific code remains behind stable runtime and rendering boundaries.

## 12. Open design questions for Codex

Work through these in order; preserve open questions rather than silently converting guesses into requirements.

1. **Compatibility baseline:** Which exact BBC BASIC V and VI versions, documented behaviors, extensions, and known quirks define source compatibility? Which programs form the representative compatibility set?
2. **Shared tokenised format:** Which record-boundary layouts, token maps, and line-reference rules are used by the earlier BASIC versions? Which version-specific language and runtime behaviors must remain distinct after the shared decoder normalizes the file?
3. **Compatibility selection:** How does a program select the 32-bit compatibility personality—source metadata, application manifest, launcher setting, or another mechanism? What should happen when no personality is declared?
4. **BASIC64 evolution:** Which new language features are essential at the start? How do integer suffixes, pointer/address types, overflow, string representation, and new syntax coexist with historical semantics?
5. **SWI contract:** Which parts of the register/calling convention, errors, flag behavior, argument blocks, and module lifecycle must remain byte-for-byte compatible? How are extensions versioned without changing existing calls?
6. **Pointer descriptors:** How are pointer-bearing SWI arguments described? What are the exact rules for buffers retained after return, callbacks, vectors, async I/O, and module-held references?
7. **Shared memory and Dynamic Areas:** What names or handles identify a shared region? Who can map it, resize it, revoke it, or free it? How are old Dynamic Area calls represented?
8. **Task and module lifetime:** Are modules reentrant? How is module workspace allocated per call or per task? What happens to retained references when a task or module exits?
9. **Address layout:** What logical ranges and alignment rules are visible in each personality? Which historical BASIC memory variables/operators must retain exact meaning?
10. **ARM compatibility scope:** Which ARM ISA generations and legacy modes are required? Is an interpreter sufficient initially, and what semantics must a translator preserve for memory and SWIs?
11. **Rendering backend — settled for the first hosted display:** use `winit` for one window and `pixels` for the compatibility framebuffer; keep the rendering boundary replaceable as the desktop composition service grows.
12. **Text profiles:** Which legacy metrics and layout behaviors can be reproduced? How does an application declare a profile, and how are profile changes prevented from breaking measurement/painting consistency?
13. **Desktop input:** How should Select/Menu/Adjust work on a trackpad or two-button mouse, and what accessibility remapping is needed?
14. **Application bundles:** What is the directory layout and manifest format? How are file types, launch behavior, permissions, updates, and resource lookup described while keeping the bundle inspectable?
15. **Trust boundary:** Which services are safe for every task, which require capabilities, and which components are trusted? How does BASIC64 system code receive privileges without giving every application access to system memory?
16. **First host target:** The initial host is macOS with a single in-window display. `--stdio` retains the terminal adapter. Which additional host systems and runtime backends should follow?
17. **Guest path syntax:** What is the complete RISC OS-style path grammar, including roots, parent/current-directory notation, device names, file-type metadata, and mapping to host paths?
18. **Clean-code eligibility:** Which operations and effects disqualify code from the clean subset, at what granularity is eligibility established, and how are dynamic calls or changed dependencies revalidated? Which BASIC64 system components are good initial workloads for this analysis?
19. **Compilation units and artifacts:** Should the first native unit be a procedure, BASIC64 module, or whole file? Does the preferred Cranelift backend meet real JIT/AOT workload needs, and how should source, classic BASIC version/profile, dependencies, runtime ABI, target, and BASIC V library lookup affect compilation and cache invalidation?

## 13. Working principles

- **Build the computer Acorn might have built in 2026.**
- **Preserve the contract, not obsolete implementation.**
- **Addresses are local. Services are global.**
- Keep documented compatibility stable; require a very good reason to change a public API.
- Keep the trusted Rust core focused on mechanisms; implement most user-facing OS policy in inspectable BASIC64.
- Make modern capabilities system-wide services so every application can benefit without selecting its own stack.
- Keep old and new execution personalities explicit when their semantics differ.
- Treat implementation details and unresolved choices as questions until evidence settles them.
