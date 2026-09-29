# Acorn-2026 / Trellis: Design Brief

Rendering validation: set `ACORN_VELLO_SNAPSHOT=1` when running the existing Filer snapshot commands to render the real Vello scene on the GPU, including an sRGB presentation round trip. This requires access to a graphics adapter; it must fail explicitly rather than silently substitute the legacy software renderer. Vello's display-encoded output must not receive a second sRGB encoding during presentation. The native backend prefers a non-sRGB surface; its sRGB-only fallback samples through a decoding view before the destination encodes.

> **Build the computer Acorn might have built in 2026.**

The environment is named **Trellis**. Its mission is to make the computer
understandable, programmable and malleable by the person using it, continuing
the Acorn tradition through a modern, live and inspectable RISC OS environment.
The live-system architecture, module-owned SWI decision, and bootstrap boundary
are specified in [`trellis-architecture.md`](trellis-architecture.md). The
implementation phases and tasks are defined in
[`trellis-work-packages.md`](trellis-work-packages.md).
The agreed native language extensions and their compatibility boundary are
specified in [`basic64-system-profile.md`](basic64-system-profile.md).

This is a design brief for a new, tinkerable computer environment inspired by Acorn and RISC OS. It is a re-imagining, not a RISC OS 3.71 simulator or a cosmetic remake. RISC OS supplies ideas and a valuable body of API knowledge; it does not dictate the implementation.

The architecture below records the current direction and identifies decisions that still need design work. It is not a claim that every detail is settled.

## 1. Project philosophy

The computer should be understandable from the first `PRINT` statement down to files, graphics, desktop services, memory, and machine code. Programming belongs inside the environment rather than behind a separate, heavyweight toolchain. A user should be able to write a small program, inspect how the desktop works, and modify system components with the same language and tools.

The guiding question for each inherited feature is:

> Does this preserve a useful Acorn idea, or merely an old implementation detail?

Preserve directness, inspectability, stable interfaces, fast startup, applications as tangible objects, a capable file manager, contextual interaction, drag-and-drop, and the sense that the computer is open to its owner. Let go of old hardware limits, pixel-era rendering assumptions, and compatibility burdens that do not serve those ideas.

The environment should feel descended from Acorn's design culture and remain crisp, restrained, content-focused, and modern. The desktop keeps classic RISC OS behavior, terminology, and source compatibility while using a new modern visual system by default. The 3.71 desktop is reference material for interaction, layout, and selected textures; reproducing its old furniture, palette, and display limits is not the visual target.

The user-approved visual target is recorded in [Desired desktop look](desired-desktop-look.md), with the [approved mockup](assets/approved-desktop-design.png). This reference supersedes earlier appearance experiments; it specifies the desired look, not completed implementation.

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
- Recreating the entire RISC OS 3.71 operating system or its appearance. The Wimp slice demonstrates source-compatible services and classic interaction patterns inside a modern desktop presentation.
- Preserving 26-bit addressing, an application-slot memory limit, a single flat application/system address space, old display hardware, or other limits solely for nostalgia.
- Promising automatic, lossless recompilation of arbitrary RISC OS binaries. The planned JIT/AOT path does include eligible BBC BASIC I–VI source and tokenised programs under their respective compatibility semantics; translating arbitrary pre-existing machine-code binaries remains separate work.
- Making every desktop component Rust code. Rust is for the trusted low-level core; BASIC64 is deliberately used above that boundary.

## 3. Architectural shape

The Rust runtime is the machine's low-level service core, not a traditional kernel. It mediates tasks, memory, service calls, input/output, and rendering through the host OS and appropriate host libraries. The BASIC64 runtime uses an interpreter as the universal execution path and an opt-in experimental JIT for verified regions. Both consume the shared parsed-program representation.

```text
┌───────────────────────────────────────────────────────────────┐
│ User programs and applications              BASIC64           │
├───────────────────────────────────────────────────────────────┤
│ Desktop, Filer, Wimp-like policy, system tools BASIC64         │
├───────────────────────────────────────────────────────────────┤
│ BASIC64 runtime in Rust: parser, interpreter, memory, REPL     │
│                  optional experimental JIT                    │
├───────────────────────────────────────────────────────────────┤
│ Global SWI dispatcher and module namespace                     │
│ BASIC64 modules wrapping protected Rust primitives             │
├───────────────────────────────────────────────────────────────┤
│ Rust runtime core: tasks, logical memory, IPC, host I/O,        │
│ graphics/text service boundary, scheduling policy              │
├───────────────────────────────────────────────────────────────┤
│ Host operating system, rendering libraries, hardware drivers    │
└───────────────────────────────────────────────────────────────┘
```

Rust owns mechanisms that need a small, dependable implementation boundary: task identity and scheduling, address translation, memory allocation and protection, service dispatch, inter-task communication, and access to host facilities. BASIC64 owns the public SWI implementations as well as most system policy and user-facing behavior: desktop rules, Filer behavior, application conventions, and higher-level services. A BASIC64 SWI definition may wrap a protected Rust primitive, but the primitive is not itself a public SWI provider.

This boundary is a starting point, not a demand to put every policy decision in one layer. Keep the trusted Rust core small enough to inspect, and make system behavior that users may reasonably want to change available as BASIC64 source.

## 4. BASIC64 and source compatibility

BASIC64 should remain recognizably BBC BASIC: immediate use, short programs, `PRINT`, `INPUT`, `FOR`, `REPEAT`, `PROC`/`FN`, `LOCAL`, `DIM`, hexadecimal constants, built-in graphics, and a path down to memory and assembly. It is a language for using and understanding the computer, not only for teaching programming.

The compatibility target is **BBC BASIC V/VI source semantics where feasible**. Existing documented source behavior should remain intact in a compatibility personality: syntax, operators and precedence, numeric and string behavior, control flow, error handling, built-in procedures/functions, memory operators, and interactions with `SYS` should be inventoried and treated as a compatibility contract. New BASIC64 capabilities should be additive or opt-in so they do not silently reinterpret established source.

BASIC64 is the system's native language; earlier BASIC syntax defines compatibility paths, not the language used to implement the system by default. A firm requirement is that the system can load programs from all earlier BBC BASIC versions. The user has described the tokenized saved-program format as shared. Real BBC Micro and BBC Master files show one shared-boundary layout; the ARM BASIC V fixture uses a separate line terminator and record marker. One decoder accepts both layouts and preserves token bytes. The Master TETRIZ 1.5 fixture adds BASIC IV-class evidence, though its exact ROM revision is unknown and it does not cover every BASIC IV-specific token. Shared-boundary execution begins with the deliberately narrow leading-`REM`, literal-string `PRINT`, and `END` common-token subset; the detected record layout does not identify an exact BASIC release, and unsupported tokens fail explicitly. Version-specific token interpretation and execution remain separate compatibility work. The current evidence and gaps are tracked in [`docs/tokenized-basic-compatibility.md`](tokenized-basic-compatibility.md). UTF-8 source (`.bas64` or `.bas`) and decoded tokenized programs (`.bbc`) now converge on the same `ParsedProgram`, compatibility interpreter, and optional Cranelift JIT. A leading `REM @BASIC64` directive can select `MODE=CLASSIC`, `MODE=BASIC64`, or `MODE=HYBRID`, and `TARGET=HOSTED`/`RISCOS` or `TARGET=AGON`; each declared directive field overrides the corresponding persisted preference, while configuration fills unspecified fields. When no directive or configured value supplies a mode or target, the runtime defaults remain `HYBRID` and `HOSTED`. The Agon target selects its modern VDP 1.04+ mode table and BBC-style 1280×1024 logical graphics coordinates in the hosted graphics service; this does not emulate the VDP protocol or firmware. The language mode still does not provide complete full-width integer semantics for ordinary BASIC64 programs; explicitly typed System Profile `INT64`/`UINT64` operations have a separate exact integer path. The first encoded compatibility fixture is ClockSP5 program version 5.08, kept beside its text source.

“100% compatible” needs a bounded definition. Source-level compatibility does not itself promise that arbitrary ARM machine code, undocumented interpreter quirks, or hardware-specific code will run unchanged. The exact BASIC V/VI baseline, edge cases, and compatibility boundary are open design questions.

In the current implementation, the directive's `TARGET` is applied to the graphics runtime, while ordinary-program `MODE` and `PROFILE` are parsed and retained as metadata only. The shared numeric representation still applies to ordinary programs; the explicitly typed System Profile `INT64`/`UINT64` subset is separate. The broader declared integer widths above remain future semantics work.

The runtime is written in Rust. Keep the interpreter as the language reference and fallback path. The experimental JIT targets verified regions of the shared parsed-program representation and must preserve program-visible behavior. Any wider compilation strategy should keep the REPL and interpreted path useful. An opt-in strict whole-program native mode may compile supported programs without entering the interpreter; unsupported constructs must either be diagnosed before execution or lower to an explicit checked runtime error when control reaches them.

### Source modes and target profiles

Text source may declare its execution personality with a compiler directive in a `REM` comment near the start of the file:

```basic
REM @BASIC64 MODE=HYBRID TARGET=AGON
```

Use one directive per file; `MODE` may be `CLASSIC`, `BASIC64`, or `HYBRID`, and `TARGET` may be `HOSTED`, `RISCOS`, or `AGON`. `CLASSIC` may include a release `PROFILE`, for example `REM @BASIC64 MODE=CLASSIC PROFILE=BBCV-1.05`. The directive is compiler metadata, not an executable statement, so classic interpreters ignore it as a comment ([BBC BASIC reference](https://www.riscos.com/support/developers/bbcbasic/bbcref.html)). It must appear before executable source. Unknown, repeated, or conflicting fields are errors. If a tokenized file preserves a leading `REM` directive, it uses the same metadata; otherwise its loader or package metadata must provide a language profile. Record layout alone is not enough to infer the originating BASIC release.

MOS execution preferences are persisted with `*CONFIGURE <option> <value>` and inspected with `*STATUS [option]`, following the RISC OS command convention. The hosted runtime stores them in a per-user configuration file instead of CMOS RAM; `ACORN_CONFIG_PATH` can select another file. `*BASIC <file>` loads and runs source or tokenized BASIC using these preferences. `BASICMode` accepts `AUTO`, `CLASSIC`, `BASIC64`, or `HYBRID`; `BASICProfile` accepts `AUTO` or a profile name; `BASICTarget` accepts `AUTO`, `HOSTED`/`RISCOS`, or `AGON`; `BASICEngine` accepts `INTERPRETER`, `HYBRID`, or `STRICT`. `AUTO` uses the file's matching `REM @BASIC64` field and then the current runtime default. A declared source field wins over the corresponding saved preference because it records the program's intended environment. Saved engine selection is separate: the directive has no engine field. The engine defaults to `INTERPRETER`; strict and hybrid JIT choices require the `experimental-jit` build.

The standard RISC OS `*CONFIGURE Language <module_no>` startup option is also supported: `0` selects the MOS command prompt, and `3` selects the desktop, matching the [RISC OS command reference](https://www.riscos.com/support/users/starcomms/index.htm) and [A3000 User Guide](https://www.4corn.co.uk/archive/docs/A3000%20User%20Guide-opt.pdf). This hosted profile currently accepts only these two module numbers; other Language values, including the historical BASIC module number `4`, are rejected. The hosted setting defaults to `0` to preserve the existing MOS-first startup. In the normal windowed frontend, `Language 3` starts the real `DESKTOP` command path and `$.System.Desktop`; `--stdio` remains a terminal recovery mode and `--desktop-demo` remains an explicit sample-app mode.

The modes describe language and type semantics. `CLASSIC` preserves the selected BBC BASIC version profile. `BASIC64` uses signed 64-bit integer variables and 64-bit floating-point values. `HYBRID` uses the selected classic profile's integer width for `%`, permits `%%` for a signed 64-bit integer, and uses 64-bit floating-point values for unsuffixed numeric variables. The exact default classic profile and whether `%%` is accepted as a redundant suffix in `BASIC64` remain open choices. One compiler pipeline should share tokenization, syntax-tree infrastructure, typed IR, interpreter, and compiler backend where semantics permit, while keeping profile-specific grammar, numeric behavior, and runtime operations explicit.

An Agon environment is a target profile, not a fourth integer-width mode. `TARGET=AGON` can be paired with any language mode; for example, hybrid source can use BASIC64's wider types while calling Agon-compatible services. Classic Agon source selects both a classic language profile and the Agon target. The target profile covers Agon's screen-mode table, graphics coordinate and colour behavior, VDU/VDP commands, and MOS services. Those effects go through runtime service interfaces, with a hosted adapter for rendering and input; board-specific GPIO access requires an explicit capability. Agon mode numbers and graphics conventions must not silently resolve through the RISC OS profile—for example, Agon's mode 20 differs from RISC OS mode 20.

The initial `TARGET=AGON` scope is to run Agon-oriented BASIC source through the hosted runtime. It does not by itself promise eZ80 code generation or bare-metal execution. An Agon compatibility profile must identify the relevant BBC BASIC variant and any required MOS/VDP API level, because the platform has multiple BASIC and firmware versions. A native Agon backend can be evaluated separately without changing the source-mode model. See the [Agon BASIC](https://agonplatform.github.io/agon-docs/BBC-BASIC-for-Agon/), [screen-mode](https://agonplatform.github.io/agon-docs/vdp/Screen-Modes/), and [MOS](https://agonplatform.github.io/agon-docs/MOS/) references.

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

Recognised parameterless BBC MOS `CALL` entrypoints are service calls: translate their BASIC register inputs and checked logical pointers directly to the caller-scoped SWI dispatcher before considering a processor compatibility executor. The first hosted bridge supports character I/O, CLI, simple file calls, a bounded OSBYTE input subset, and OSWORD 1–4 clocks. BASIC `TIME` and OSWORD system-clock operations share dispatcher-session clock state rather than restarting TIME for each interpreted program. Legacy file control-block adapters, timer events, arbitrary machine code, and native JIT statement lowering remain separate work. The supported addresses, argument conventions and limits are recorded in [Hosted BBC MOS CALL bridge](mos-calls.md). This resolves the service boundary for legal MOS entrypoints without broadening the runtime into a CPU emulator.

The eventual JIT/AOT path is not limited to BASIC64. It should compile eligible programs for every classic BBC BASIC compatibility personality the runtime supports, with BBC BASIC I–VI as the long-term coverage goal. Classic source and decoded tokenised programs should lower through their selected version profile, not be silently reinterpreted as BASIC64. A tokenised file's record layout alone does not establish its language version; the selected compatibility profile and its semantic version must be part of compilation and cache identity.

For each implemented classic profile, compiled execution must preserve the interpreter's observable numeric model, operators, control flow, variables and arrays, built-in behavior, errors, memory operations, and SWI effects. Operations with understood semantics can lower to the shared IR or call checked runtime helpers. Unsupported or opaque operations can remain interpreted or use a target-specific compatibility executor while eligible procedures compile, so one such operation need not disqualify an entire program. Embedded ARM or 6502 machine code remains a distinct execution target; compiling BASIC source does not translate arbitrary machine-code routines into host instructions. Compiled artifacts should retain source or tokenized-line locations and record the compatibility profile, runtime ABI, dependencies, and host target used to build them.

### BBC BASIC V procedure libraries

BBC BASIC V's `LIBRARY` and `INSTALL` commands load separate saved BASIC programs that normally contain `PROC` and `FN` definitions. RISC OS BASIC V also has `OVERLAY` libraries, loaded on demand when a named procedure or function is called. Procedure lookup searches the main program first, then `LIBRARY` files (most recently loaded first), `INSTALL` files (in reverse load order), and finally the overlay list. These are name-searched procedure libraries rather than isolated modules with a declared ABI: resolution depends on the active library set, and library code may rely on the caller's BASIC variables and runtime state. The guide also warns that line-number references in a library refer to the main program, and recommends keeping library routines self-contained. Preserve these loading and lookup rules in the compatibility personality. They can still be compilation inputs: analyze and compile individual resolved definitions when eligible, while retaining runtime name resolution and invalidating compiled entries when the active definition or its dependencies change. Do not treat a library file's boundary as proof that all its routines are clean or statically closed. The [BASIC V procedures and libraries guide](https://www.riscos.com/support/developers/basicv/chap05.htm) describes the library model and its `LIBRARY`, `INSTALL`, and `OVERLAY` behavior.

### Additive native language evolution

The native BASIC64 System Profile will add the bounded facilities required to
implement Trellis clearly: modules and visibility, named records, enums and
flags, typed definitions, structured errors, opaque handles, read-only
bindings, declarative SWI/primitive metadata, and distinct managed-reference
and logical-address types. These are settled feature categories; their exact
grammar and detailed semantics are preparatory work defined in
[`basic64-system-profile.md`](basic64-system-profile.md).

The additions are explicitly profile-gated and must not silently change BBC
BASIC V/VI source semantics. Classes, inheritance, generics, macros, universal
message dispatch, and async syntax are deferred until executable system work
demonstrates a requirement. First-class functions and closures are likely but
do not block the first module slice unless a concrete callback or lifecycle
case requires them.

Keep the language concise and direct. Avoid making BASIC64 a syntax-heavy
systems language or requiring a large framework for ordinary programs.

## 5. Services, SWIs, and modules

### Keep the service contract stable

API compatibility is a central constraint. Preserve familiar service names, `SYS` usage, module discovery/registration concepts, argument and result conventions, error behavior, and documented SWI semantics wherever feasible. Change an externally visible contract only for a clear reason, document the incompatibility, and provide an adapter or compatibility route where practical.

Do not confuse the historical implementation with the public contract. A SWI can keep its name and calling behavior while its handler uses Rust-owned task contexts and translated memory access rather than dereferencing a process-wide host pointer. BASIC64 may add higher-level APIs, but those should sit alongside the stable service surface.

### One namespace, module-owned implementations

The dispatcher exposes one global SWI namespace. Every public SWI is exported
by a module and implemented by a versioned BASIC64 definition. That definition
may implement the service, alias another definition, or wrap a private,
capability-protected Rust primitive. Rust hard-codes no public SWI name, number,
or semantic handler.

```text
                  Global SWI namespace
                           │
                 BASIC64 module definition
                           │
             BASIC64 service, alias, or wrapper
                           │
              protected Rust primitive if needed
                           │
                  host or machine mechanism
```

Rust owns dispatch, caller context, logical-memory translation, capability
enforcement, module lifecycle, and execution machinery. Module workspaces belong
to modules; parameters supplied by an application belong to the calling task.
The dispatcher carries caller identity across the whole call chain.

The initial SWI environment is supplied by BASIC64 foundation modules loaded
from a trusted boot capsule. Before they are published, no SWI environment
exists. Native bootstrap operations and emergency diagnostics use a private
primitive interface rather than special hard-coded SWIs. See
[`trellis-architecture.md`](trellis-architecture.md) for the normative bootstrap
sequence and module model.

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

The hosted frontend retains one `winit` window and renders through `wgpu` and Vello. Vello composes modern Wimp furniture and system content at the host surface's physical size. Classic guest graphics stay CPU-authoritative in separate task-default or Wimp-window RGBA surfaces, which are uploaded as images for composition. This is an incremental migration: classic content remains on its mode-sized pixel grid, while indexed colour and full historical drawing semantics remain incomplete.

Window keyboard bytes feed the hosted console input source. The runtime reads them on its own thread through `OS_ReadC` and `OS_ReadLine`, while display events update the window's graphics scene. This allows a synchronous BASIC `INPUT` statement to wait for a line without stopping window event processing; visible text continues to use the normal SWI output path.

### First compatibility graphics slice

The hosted compatibility path uses a stateful VDU stream at `OS_WriteC`, the standard `OS_Plot` SWI (`&45`), and `OS_ReadPoint` (`&32`) for synchronous CPU-raster reads. `OS_WriteS`, `OS_Write0`, and `OS_NewLine` continue to reach the same byte stream through `OS_WriteC`. The stream parser retains partial VDU commands across calls, so the command byte and its parameters may arrive separately. The contracts follow the [RISC OS VDU driver](https://www.riscos.com/support/developers/prm/vdu.html) and [VDU code table](https://www.riscos.com/support/developers/prm/vducodes.html).

The initial BASIC V/VI surface adds `MODE`, `VDU`, `LINE`, `MOVE`, `DRAW`, `PLOT`, `GCOL`, and `PRINT TAB(x,y)`. `MODE` and `VDU` send their control bytes through `OS_WriteC`; `LINE` and the related plot statements use `OS_Plot`. The hosted mode table follows the standard numbered entries in [RISC OS PRM Table B](https://www.riscos.com/support/developers/prm/modes.html): modes 0–46 except unassigned mode 32, plus the BBC BASIC shadow aliases 128–164 for base modes 0–36. It carries each mode's text grid, pixel dimensions, OS-unit graphics extent, colour count, and pixel depth; it exposes the table as a deterministic hosted set without monitor-timing restrictions. Modes 3 and 6 are text-only. Mode 7 uses a 40×25, 480×500 Teletext surface with row-scoped colour/background controls, flashing, double-height text, and sixel mosaic graphics. The service also models text and graphics windows, graphics origin and cursor, basic colour state, a text-cell surface, and retained line/point primitives. It handles VDU 12, 17, 18, 22, 24, 25, 26, 28, 29, 30, and 31, plus common text cursor controls. A selected 32-bit extended mode descriptor is also supported with its pixel dimensions and X/Y eigenfactors. The BASIC syntax follows the [simple graphics](https://www.riscos.com/support/developers/bbcbasic/part2/simplegraphics.html), [complex graphics](https://www.riscos.com/support/developers/bbcbasic/part2/complexgraphics.html), [Teletext mode](https://www.riscos.com/support/developers/bbcbasic/part2/teletext.html), and [VDU control](https://www.riscos.com/support/developers/bbcbasic/part2/vducontrol.html) chapters. The host does not emulate monitor timings, shadow memory banks, full palette/ColourTrans behavior, or every Teletext attribute. Other VDU commands are not claimed as implemented merely because the stream parser consumes their documented parameter count.

Each classic mode now owns a persistent, mode-sized CPU RGBA raster, including the selected extended 32-bit C16M mode. Plot calls update that raster synchronously, so a high-volume program does not retain one scene primitive per plotted pixel. Display snapshots share each destination raster and continue to publish at most about 60 times per second during `BASICRUN`. Mode and default-palette metadata are retained, but the RGBA interim surface does not preserve every indexed logical colour, tint or plot action; palette mutation and fully exact indexed readback remain compatibility gaps. The selected C16M descriptor and the `ColourTrans_ConvertHSVToRGB` and `ColourTrans_SetGCOL` calls used by the full Mandelbrot listing are supported; this is not a general RISC OS mode-selection or ColourTrans implementation. `cargo run` opens the window; `cargo run -- --stdio` keeps the terminal host available for command-line use. Legacy saved-file decoding remains shared, but execution semantics are selected by the compatibility profile. Graphics support does not imply machine-code execution. `CALL` needs a separately selected processor compatibility service; no 6502 emulator is introduced by this graphics work.

The first end-to-end display milestone accepts `HELP` and `QUIT` at the in-window MOS prompt and runs [`examples/graphics/text-and-pixels.bas`](../examples/graphics/text-and-pixels.bas) from its tokenised companion file. The program prints text and plots points on the same framebuffer. This is an initial compatibility display, not the completed desktop graphics service.

In the normal windowed frontend, the case-insensitive `DESKTOP` MOS command (with final-dot abbreviations) hands that same host window from the prompt to the shared Wimp service, initially showing an empty 800×600 desktop. The command suspends its caller while the Wimp surface is active, so the MOS task does not keep consuming keyboard input or repainting prompts behind it. Closing the host window stops the service and releases the suspended task; `--stdio` reports that a graphical host is required. `--desktop-demo` remains a separate startup path for the two BASIC Wimp examples. This bootstrap command is Rust prompt policy for now; the long-term desktop and application policy remains in BASIC64.

The authentic TDU-01 file remains the working source for the common graphics slice and useful evidence for a real BBC Micro saved-program layout. Develop the language-level `MODE`, VDU, and drawing behavior it contains through the standard SWI services. TDU-01 itself is not a required release-compatibility or whole-program acceptance target: its procedure flow, direct memory operations, and machine-code routines extend beyond this slice. The source-derived Mandelbrot fixture exercises its selected extended C16M path through generated ARM BASIC V tokens; this is a program-specific integration case, not broad BASIC V/VI or RISC OS compatibility. The compatibility corpus records these distinctions in [`docs/tokenized-basic-compatibility.md`](tokenized-basic-compatibility.md).

### Font sources and rendering

The BBC Micro compatibility display uses the supplied 8×8 MOS character bitmap for character codes 32–127. Preserve these glyph bytes and draw them directly into the shared framebuffer for classic BBC text modes; this bitmap is distinct from the RISC OS desktop fonts. Codes below 32 remain VDU control characters, and handling of additional character sets can be added when a real program requires them. The table is available to the renderer in [`src/font.rs`](../src/font.rs). The classic adapter keeps `render()` on the BBC path. The Vello shell uses bundled Inter Variable through Parley for modern UI text. Text-only guest displays inside Wimp windows still use the Homerton compatibility rasterizer and retain their original character grid and cursor positions; the MOS/BBC compatibility display remains pixel based. Pinned System bitmap resources remain available for future explicit compatibility profiles.

RISC OS system faces such as Corpus, Homerton, and Trinity use their native Font Manager resources as the authoritative assets: `IntMetrics`, `Outlines`, encoding data, and any supplied size-specific bitmap files. Read the native metrics and outlines, rasterize glyphs for the requested size, and cache the resulting bitmaps for drawing. This keeps the Acorn outlines and spacing while avoiding outline rasterization on every frame. The hosted rasterizer supports the pinned version-8 outlines in these three families, uses supersampled even-odd fills, and falls back to the native `?` metric for characters outside the included Latin-1 mapping. This is a resource and renderer capability, not a guest `Font_*` service. Do not make TrueType/OpenType conversion a required asset-preparation step; generated derivatives may be used for interoperability or comparison, but are not the source of truth. The original assets' source commit, file paths, blob identifiers, and rights note are in [`resources/riscos-3.71/README.md`](../resources/riscos-3.71/README.md).

The paths remain selectable by rendering profile: BBC compatibility text uses the fixed 8×8 bitmap; RISC OS system text uses Font Manager metrics and scalable outlines. Both remain available to the compatibility renderer. The ROM outline fonts cover the included Latin-1 map and provide a distinctive Acorn voice, but they do not provide modern script shaping, full Unicode coverage, or advanced typographic features. The desktop uses Inter through the system-owned shaping and fallback path described below.

Text is a first-class graphics primitive alongside paths, images, surfaces, transforms, clipping, and paint. The system owns text shaping and measurement so applications do not each choose a separate text stack.

```text
Text and font request
        ↓
Unicode/script segmentation and bidirectional analysis
        ↓
font fallback and OpenType shaping through Parley and Fontique
        ↓
glyph positions and layout metrics
        ↓
rasterization and composition through host rendering facilities
```

Modern shaping should support complex scripts, combining marks, ligatures, bidirectional text, font fallback, variable fonts, and modern Unicode. The current shell uses Parley with bundled Inter Variable; this does not yet provide a guest text-measurement or font SWI service, and fallback coverage depends on fonts available to the host. Applications should ask the system to measure, lay out, and draw text without depending on shaping-engine internals.

Preserve the Font Manager concept and familiar SWIs such as `Font_FindFont`, `Font_Paint`, and `Font_StringWidth`. Their names and calling contracts remain stable where feasible. Modern implementations can provide better shaping beneath them.

Compatibility belongs in profiles, not a normal global shaping switch. Legacy applications can receive text metrics and behavior appropriate to their compatibility profile; new BASIC64 applications and the desktop use modern shaping and font fallback by default. The current shell uses Inter; text-only guest displays can still use the Homerton compatibility path. A developer-only setting may force profiles for diagnosis. Measurement, wrapping, caret placement, hit testing, menu sizing, and painting must agree within each profile.

## 9. Desktop, Filer, and application model

The current visual reference is the flat, square desktop mockup: charcoal window rules, compact rectangular controls, a warm yellow active title, neutral inactive furniture, simple outlined Filer icons, and a quiet grey desktop. At the 1600×1200 backing resolution, furniture uses 2-sample (one logical pixel) borders, 48-sample titles and matching 48-sample scrollbars and a 100-sample icon bar. Painting and hit testing share the Wimp geometry. These are hosted presentation dimensions, not changes to guest SWI argument formats. Menu clicks over file icons retain button state 2 regardless of icon button type, as required by the [Wimp Mouse_Click contract](https://www.riscos.com/support/developers/prm/wimp.html); they do not enter Select/Adjust double-click encoding. The sample editor, calculator, extra volumes and clock in the reference are not additional application requirements. Horizontal scrollbars remain outside the implemented Wimp slice; the bottom resize strip must not pretend to be a working horizontal scrollbar.

The desktop is a BASIC64 system made from inspectable, modifiable components. The Filer is a central part of the environment; applications communicate through defined messages and shared services instead of relying on deep coupling. A Wimp-like event and window model can provide the familiar conceptual interface while the Rust core handles low-level window, input, and graphics mechanisms.

### Guest paths and host paths

Guest paths use RISC OS FileSwitch syntax and are parsed independently of host paths. The first hosted filing system is `HostFS`, with the checked-in folder `demo-volume` mounted as the `DemoDisk` volume by default. `ACORN_DEMO_VOLUME` can select another host folder. A full example is `HostFS::DemoDisk.$.Examples.ClassicSmoke`; `HostFS:` selects the same filing system without naming the volume, and a bare guest path is relative to the task's current directory. The first implementation recognizes `$` (volume root), `@` (current directory), `^` (parent), `&` (user root), `%` (library directory), and `\` (previous directory). A dot separates guest path elements, so host extensions are not guest filename syntax.

Host payloads remain ordinary files and directories. Per-file catalogue data is stored in a sibling regular text file named `<host filename>.acornmeta`; volume identity is stored in the regular root file `.acorn-volume`. Both formats are versioned, human-readable, and checked into source control. They are portable files, not host extended attributes, and FileSwitch hides them from guest catalogues. A sidecar records the guest leaf name, file type, load address, execution address, and attributes. File types are stored as a 32-bit value so the metadata format can grow; classic RISC OS FileSwitch calls keep their documented 12-bit file type fields. The standard `&FFB` BASIC type selects tokenized BASIC loading; other types are treated as UTF-8 BASIC source by the current `RUN` command.

File access is exposed through standard numbered SWIs: `OS_File` (`&08`), `OS_Args` (`&09`), `OS_BGet` (`&0A`), `OS_BPut` (`&0B`), `OS_GBPB` (`&0C`), `OS_Find` (`&0D`), and `OS_FSControl` (`&29`). `HostFS` uses filing-system number 1 in this hosted profile; that number is unallocated in the classic FileSwitch table. The hosted slice implements the common catalogue, create, load/save, file-type, byte-stream, block-transfer, directory-enumeration, current/library/user-root directory, filing-system selection, rename, and volume-name reason codes. Open handles and current-directory state belong to a task. `OS_CLI` dispatches the initial filing commands (`*CAT`, `*DIR`, `*CDIR`, `*DELETE`, `*RENAME`, `*FILETYPE`, and `*TYPE`) through those same SWI handlers. Command names are case-insensitive; a leading abbreviation must be terminated with a full stop (for example, `*CA.` for `*CAT`), while full names need no terminator. The FileSwitch shorthand `*.` also catalogues the current directory. Abbreviations select the first matching built-in command in the dispatcher order; aliases and dynamically installed commands are not implemented. `RUN` and `BASICLOAD` resolve guest paths and use sidecar file types rather than host path extensions. This is an initial FileSwitch-compatible service, not a claim of complete FileSwitch compatibility: File$Path/Run$Path search variables, every reason code, full error-block identity, and all host-name mapping policies remain future work.

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
| Drag-to-save and tangible applications | Old screen geometry and exact whole-desktop reproduction |
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

Build the BASIC64 interpreter and compatibility execution around one internal program representation. UTF-8 `.bas64` and `.bas` files and decoded `.bbc` saved programs feed the same statement engine; the opt-in Cranelift JIT compiles eligible regions from that same representation. `BASICLOAD` decodes the observed shared-boundary and separate-terminator saved-program layouts, preserves token bytes, extracts line references, and retains a loaded program. The corpus and its version evidence are recorded in [`docs/tokenized-basic-compatibility.md`](tokenized-basic-compatibility.md); this decoder coverage does not yet establish every earlier BASIC release. `*BASIC <file>` loads and runs a source or tokenized BASIC program; `*CONFIGURE` and `*STATUS` set and inspect persisted, per-user execution preferences in the MOS command style ([`*BASIC`](https://www.riscos.com/support/developers/prm/basic.html), [`*Configure` and `*Status`](https://www.riscos.com/support/developers/prm/memoryman.html)). The hosted preference file is the CMOS-like persistence layer. `RUN`, `BASICRUN`, and BASIC applications launched in the desktop also use these settings. The configured engine defaults to the interpreter. A leading `REM @BASIC64` directive takes precedence over configured language, target, or profile values for the fields it declares; configuration supplies the remaining fields. Engine selection is independent, since the current directive syntax does not specify an engine. The user configuration is read for each program run so edits apply to subsequent runs. The configuration values and their precedence are defined under [Source modes and target profiles](#source-modes-and-target-profiles). `BASICRUN` therefore uses the interpreter with defaults, and follows another configured engine when the user selects one. The hosted profile supplies monotonic centisecond `TIME` and nonblocking host-key `INKEY` (`-256` for bare `INKEY` and `-1` for `INKEY(0)` when no key is pending); ClockSP5's own guards skip native ARM and hardware setup, while its final hardware reset command is a no-op. This is a program-specific acceptance milestone, not general BASIC V/VI execution or a measurement of the host processor's physical clock. A source-derived Mandelbrot fixture separately exercises parameterized routines, indirect strings/words, the selected C16M extended mode block, and the two `ColourTrans` calls used by that listing; this remains sample-specific coverage, not a general BASIC V/VI or RISC OS profile. Continue with the REPL, full global service dispatcher, caller contexts, task/module/shared/system memory classes, checked pointer translation, and retained-reference rules. Build the version-aware compatibility execution path for all earlier BASIC versions. Make logical memory part of the interpreter from its start.

**Exit:** native BASIC64 programs can run interactively and call services through the same caller-aware context without exposing host pointers as BASIC addresses, and tokenised programs from all earlier BASIC versions can be loaded through the shared file-format decoder and a version-aware compatibility path.

### Phase 3 — BASIC64 modules and desktop foundation

Load BASIC64 modules through the shared namespace. Establish messaging, windows, input, the Filer, file-type registration, and inspectable application directories. Put higher-level policy in BASIC64.

The normal hosted frontend defaults to the MOS prompt (`Language 0`). With `Language 3`, startup enters the same `DESKTOP` command path directly. The command activates the shared `WimpServer` in the existing host window and starts the BASIC64 system component `$.System.Desktop`; it does not start sample applications. The command can be abbreviated with a final dot, and the existing `*D.` abbreviation continues to select `DIR`. The desktop component creates a volume icon for the mounted HostFS volume. Each activation of that icon starts an independent BASIC64 `$.System.Filer` task at the volume root; each Filer window can be closed separately. Its directory listing comes from a checked catalogue extension over the caller's existing HostFS context, with parent navigation (including Adjust-close to parent except at the volume root), a vertical Wimp scrollbar, responsive icon-grid reflow after resize, and up to 56 entries per catalogue page. Horizontal scrolling remains unimplemented; the Filer reflows to the visible work width. Directory, BASIC, and file entries keep standard Wimp selection behavior; the BASIC64 Filer supplies its own high-resolution RGBA folder, BASIC-program, and document art. It uses cascading Wimp menus with a grey title and white body: the main menu offers Display, a contextual File/Dir/Selection submenu, Select all, Clear selection, Options, New directory, and Open parent. Display offers Large icons, Small icons, Full info, and checked sorts by name, type, size, and date. Sorting applies to the whole catalogue before the 56-entry page is chosen, while a bounded 56-record working set keeps catalogue item IDs stable for selection and hit testing. Name order is the default; type sorts by numeric file type, while size and date sort largest and newest first, respectively. Select replaces the selection, Adjust toggles an entry, Select on blank work area clears it, and contextual menus preserve the current selection. A per-catalogue selection map supports item IDs up to 65,534, and selection remains attached to catalogue items across display and sort changes. Open parent is shaded at the volume root. File mutations, Filer option changes, and directory creation are not implemented and their menu items remain shaded. Full info currently displays name, type, and size; it does not display dates. Rectangle/rubber-band selection is not implemented. Backspace opens the parent directory, `[` and `]` change catalogue pages, and Return opens the selected entry. The Filer updates its standard Wimp work extent as it changes pages and directories. Wimp redraw requests now follow `Wimp_RedrawWindow`, `Wimp_UpdateWindow`, and `Wimp_GetRectangle`; each Wimp window now has an independent graphics context and surface, which the Vello scene samples only for that window. The default shell has a flat, high-density style: a uniform medium-grey desktop, square dark-edged windows, quiet grey inactive and saturated yellow active titlebars, compact rectangular controls, white document surfaces, pale neutral Filer surfaces, and small flat icons in restrained bright colours. Shell labels use bundled Inter through Parley; guest text uses fixed character cells for a monospaced layout. The bottom icon bar has a light-grey surface and thin dark top edge, keeps the mounted `DemoDisk` at the far left, and places the upside-down green-and-ochre Acorn OS mark at the far right. Application icons appear immediately to the left of the OS mark only when an application explicitly creates an iconbar icon; opening a window does not create one. The centre stays quiet, with no clock. The owner-provided RO 3.71 wallpaper and work-area texture crops remain archived for reference, with provenance and reuse limits recorded in `resources/riscos-3.71/README.md`, but are not part of the default desktop. The Filer opens supported BASIC source and tokenized programs through `Wimp_StartTask`, the normal guest-file loader, and the same saved execution preferences and source directives used elsewhere. The working directory of a launched program is the guest directory that contains its file. `--stdio` keeps the terminal prompt available regardless of the saved startup language; `--desktop-demo` continues to start both example tasks directly.

The first desktop vertical slice uses separately executing BASIC guest tasks with windows in one hosted desktop surface and one shared Wimp service. The service owns globally unique task, window, and icon handles, stacking, and input routing; each caller's handles and guest-memory pointers remain task-scoped. It implements the standard numeric/name SWI entries and register/block shapes for `Wimp_Initialise`, `Wimp_CreateWindow`, `Wimp_CreateIcon`, `Wimp_DeleteIcon`, `Wimp_SetIconState`, `Wimp_SetExtent`, `Wimp_OpenWindow`, `Wimp_CloseWindow`, `Wimp_Poll`, `Wimp_RedrawWindow`, `Wimp_UpdateWindow`, `Wimp_GetRectangle`, `Wimp_ForceRedraw`, `Wimp_GetWindowState`, `Wimp_CloseDown`, and the command-string input to `Wimp_StartTask`, following the RISC OS Programmer's Reference Manual ([Wimp chapter](https://www.riscos.com/support/developers/prm/wimp.html)). `Wimp_CreateIcon` and `Wimp_DeleteIcon` support direct text and indirected text-plus-sprite icons on windows and the icon bar. `Wimp_SetIconState` supports the standard selected-state flag used by Filer entries; `Wimp_SetExtent` accepts the standard 16-byte work-area rectangle and rejects extents that exclude the visible work area. Window definitions remain narrow: zero initial icons, direct or checked indirect plain text titles (copied at creation; live title-buffer updates remain unsupported), bounded on-screen geometry above the icon bar, no poll-word flags, and work-area button types 0, 3, and 10. RISC OS 3 control flags drive Back, Close, Toggle Size, vertical scroll, and Adjust Size furniture. The compact titlebar places Back at the far left, Close immediately after it, the title in its own wide area, and Toggle Size at the far right; their shared furniture rectangles also define their hit targets. Back changes stack order; toggle and resize follow the `Open_Window_Request`/`Wimp_OpenWindow` exchange; clicking Close immediately removes that window from the desktop and queues the standard `Close_Window_Request` event so the guest can still perform cleanup. A BASIC task that does not poll events cannot keep its window open. Host-handled scrollbar arrows move by fixed steps and page regions move by two-thirds of the visible area to keep adjacent catalogue rows reachable, while thumb drags reopen the window at the selected offset. Guest Wimp coordinates stay in OS units and shared paint and hit-test rectangles convert them to the hosted 2× desktop framebuffer density. The compact frame uses crisp vector controls and Inter text; original System bitmap glyphs and pinned Tools3d sprites remain available to compatibility-oriented profiles. For this demonstration the host assigns keyboard focus when a window is clicked; RISC OS caret and writable-icon services are not implemented.

Hosted menu support implements the standard `Wimp_CreateMenu` and `Wimp_GetPointerInfo` SWIs and delivers `Menu_Selection` as poll reason 9 with the zero-based selection path terminated by `-1`. A menu block contains its title, palette, dimensions, and 24-byte item records; item text can be direct or indirect. Ticks, separators, shaded rows, and eager submenu trees are supported. The pointer opens a submenu immediately over its arrow gutter or after a short delay over the row body; cascading panels remain on the desktop surface, keep their parent row selected, and flip/clamp at screen edges. Select closes the tree, while Adjust reports the same path and keeps it available when the task reopens the menu before its next poll. Select-drag on a menu title moves the tree. Parsing is bounded to eight levels, 64 rows per menu, and 1024 rows per tree, and rejects cyclic or invalid guest pointers. Writable items, lazy `MenuWarning` submenus, and submenu references to dialogue-box windows are not implemented; these unsupported forms are rejected. This follows the PRM menu and pointer contracts while keeping guest buffers task-scoped and checked.

Hosted tasks now receive a 1 MiB checked logical memory region rather than the prototype's 64 KiB. The BASIC heap still starts at the same address; existing addresses and memory/SWI checks are unchanged. The larger initial region accommodates the BASIC Filer's RGBA artwork, menu blocks, filename buffers and catalogue selection state without introducing host pointers or a historical application-slot limit. Dynamic slot resizing remains future work.

The additive `Wimp_CreateIconEx` extension is at SWI `&4FF02`; standard `Wimp_CreateIcon` and its 36-byte definition block are unchanged. The extended definition is 56 bytes: the original block occupies bytes 0–35, followed by version `1` at offset 36, a caller guest-memory pointer to tightly packed straight RGBA8 at offset 40, width and height in source pixels at offsets 44 and 48, and source scale `2` at offset 52. Three zero image fields omit custom art. Images are copied through checked task memory, capped at 512×512, and held by the hosted icon until it is deleted. This named extension is the initial answer to versioning icon-resolution options while preserving existing source calls; it is not a historical RISC OS SWI.

`Wimp_StartTask` accepts only a `BASIC <guest-path>` command in this hosted profile and returns a Wimp handle while the host creates an isolated guest runtime. Wimp_StartTask and opening a window do not create an application icon. Applications create iconbar icons explicitly; activatable application icons appear immediately left of the OS mark and raise their target task when selected. An explicit icon for the task owning the frontmost window carries a small accent marker. A program that does not initialize Wimp receives a task-owned output/input window using its normal display snapshot and input channel. Once it initializes Wimp, the fallback window is removed so the program can use its own windows. Task exit removes its windows and owned iconbar icons; loading and runtime errors appear in a dismissible desktop notice. The BASIC64 system sources live under `demo-volume/System` and the initial sample corpus is in `demo-volume/Examples`.

`Acorn_Desktop` is a project-specific named SWI, not a historical RISC OS call. Action 1 takes a caller guest directory path in R1, zero-based catalogue index in R2, guest name buffer in R3, and buffer size in R4; it returns entry kind in R0 (0 end, 1 tokenized BASIC, 2 directory, 3 text/source, 4 unsupported), file type in R1, and length in R2. Action 2 takes a guest output buffer in R1 and capacity in R2 and returns the mounted volume-name length in R0. Action 3 takes a caller guest directory path in R1 and zero-based catalogue index in R2; it returns availability in R0 (1 available, 0 absent/unrepresentable) and the host modification time in whole Unix seconds in R1. It is an additive hosted metadata query for Filer date sorting, not the historical RISC OS five-byte timestamp contract. Existing actions are unchanged. All actions validate task memory and use the existing FileSwitch path and HostFS context.

This milestone does not claim full Wimp compatibility. `Wimp_RedrawWindow`, `Wimp_UpdateWindow`, and `Wimp_GetRectangle` use checked 44-byte blocks, visible-rectangle iteration, work-area and screen-coordinate conversion, and occlusion subtraction; supported `Wimp_ForceRedraw` forms mark work-area regions invalid for later visible redraw. Redraw clears the returned region through that window's graphics state unless the work-area background byte is `&FF` (transparent); UpdateWindow preserves the window raster. Invalid regions stay pending while hidden and are queued when stacking, closing, or opening exposes them. Extent growth also invalidates newly added regions. Windows marked as fully redrawn by the Wimp do not receive application redraw events. Each task has a default graphics context and each Wimp window receives its own context and CPU raster. A redraw/update rectangle loop routes graphics output and `OS_ReadPoint` to that window's context and clip; ending the loop restores the task-default context. No general window-selection SWI has been added. Surface allocation is bounded to 8,388,608 guest pixels per task. `OS_ReadPoint` reads the CPU raster synchronously; for indexed modes it estimates the nearest logical palette entry from RGBA, so duplicate colours, palette changes, full tint values and unsupported plot actions are not exact. C16M reads return the hosted packed RGB value. Palette mutation and complete indexed-colour storage remain follow-up work. Transparent work areas are not yet transparent in the Vello scene, whose document surfaces remain opaque. The snapshot remains at the guest screen mode's fixed pixel scale as its window is resized. The BASIC64 Filer and notices are policy; app icons can use original `Sprites22` or caller-supplied extension art. Original RO wallpaper/work-area texture crops remain archived for reference, but are not used by the default shell. Filer listings page through 56 entries at a time. The first slice does not mutate files, register general application packages, provide a desktop task manager, or implement orderly desktop shutdown. Other unsupported areas include indirect/sprite window titles, caret services, poll-word waiting, horizontal scrolling, off-screen windowing, and the remaining mouse button types. A Menu click over the work area is reported; a Menu click on system furniture is ignored. Consult the PRM for the full standard contracts; unsupported forms are rejected rather than assigned a new meaning. The BASIC demo sources are loaded from `examples/wimp/two-windows` at launch and may be edited without rebuilding the Rust host.

The hosted tasks currently execute on separate OS threads and can run simultaneously. `Wimp_Poll` blocks or yields only its calling thread; it is not yet a cooperative scheduler and does not reproduce RISC OS task scheduling. This is an explicit hosted execution deviation while task contexts and service ownership remain separate.

**Exit:** a BASIC64 desktop component can be inspected, modified, loaded, and communicate with an application through published services. The two-task Wimp demo is an early runnable milestone toward this exit, not completion of the desktop phase.

### Phase 4 — Graphics and typography

The hosted window uses `winit` with `wgpu` and Vello 0.10 for host-scale composition. Vello renders the shell's paths, shadows, icons, text and Wimp-managed modern content at the physical surface size; Parley 0.11 shapes modern shell text using bundled Inter Variable. HiDPI resizing changes the desktop-to-surface transform but does not change guest `MODE`. Classic display output is synchronously rasterized into a separate mode-sized CPU RGBA surface for the task-default destination or active Wimp window. Snapshots turn those surfaces into Vello images at presentation time, with no GPU readback for classic pixel access. Modern Wimp furniture and system content use Vello paths, text and images and have no synchronous CPU readback contract. The compatibility raster retains mode/palette metadata but is not authoritative indexed storage, and the RGBA readback can only estimate logical colour/tint where entries alias or draw actions are unsupported. The software renderer remains the compatibility image producer. The renderer pins compatible `wgpu` dependencies, but Vello 0.10's compute renderer is experimental and requires a compute-capable adapter; there is no software-compositor fallback or full device-loss recovery yet. The native build and service tests do not substitute for a visual GPU run, HiDPI verification, or a benchmark.

The compatibility renderer keeps the supplied BBC 8×8 system bitmap and the native Corpus, Homerton, and Trinity Font Manager resources. Text-only guest windows still use the Homerton compatibility rasterizer. The modern shell uses Inter and Parley; this does not add guest `Font_*` SWIs or complete Unicode/fallback guarantees. The Wimp redraw API provides visible clip rectangles and separate clear-versus-preserve behavior, with independent task-default and Wimp-window graphics contexts. `OS_ReadPoint` reads the active CPU raster before presentation. Remaining graphics work includes indexed backing and exact palette/tint/plot-action semantics, palette mutation, and full transparency handling; modern font fallback, actual-scale visual and HiDPI verification, and lifecycle recovery also remain incomplete.

The hosted window accepts system clipboard text through Command+V on macOS (Control+V on other hosts). Pasted text follows the same input channel as keystrokes: CR, LF, and CRLF line endings submit lines through `OS_ReadLine`; tabs become spaces, printable ASCII is retained, and unsupported characters are ignored. This keeps console input inside the existing SWI path.

**Exit:** the desktop and BASIC64 programs use the same service for text and graphics, with profile behavior selectable per application.

### Phase 5 — Compatibility depth

Expand BBC BASIC V/VI source coverage and the stable SWI surface according to the compatibility matrix. Use the staged MAL implementation for earlier portable source/runtime coverage. Bring in WimpLib and the Archimedes Notify source when the corresponding RISC OS Wimp/SWI surface is ready; compatibility with those programs is a goal, especially for Wimp code. Add an initial hosted Agon target profile over the shared compiler and runtime service boundary, keeping Agon graphics/MOS semantics distinct from RISC OS and recording the supported BASIC and firmware versions. Keep source behavior, converter-produced token streams, and saves from a named interpreter release as distinct evidence. Add legacy Dynamic Area behavior and, if justified by target programs, the ARM execution/translation layer for assembler. The source candidates and their provenance, licensing, and stage limits are recorded in `docs/tokenized-basic-compatibility.md`.

**Exit:** compatibility is measured against explicit source and service requirements, with known unsupported cases documented.

### Phase 6 — JIT and performance

Expand the JIT behind the interpreter's established semantics and logical-memory interfaces. Preserve the interpreter as a debugging and reference path. Keep compatibility coverage separate from performance claims: profile procedures from representative BASIC64, classic BASIC, and desktop programs, then use hot eligible computation regions as JIT workloads. Route unsupported or OS-facing operations, including Wimp/SWI calls, through checked runtime callbacks or interpreter fallback. Use the MAL and WimpLib sources to widen semantic coverage and exercise fallback; do not treat their overall runtime as a numeric JIT benchmark without profiling. Evaluate clean-code analysis as a way to identify code suitable for JIT or, later, native compilation.

An isolated feasibility prototype parses a ClockSP5-derived ARM BASIC V fixture with the compatibility parser, lowers its integer assignments and nested `REPEAT`/`UNTIL` loops into a small backend-neutral typed IR, and maps that IR to Cranelift JIT and object code in `tools/basic-jit-bench`. The MOS prompt also has an opt-in `BASICJIT` hybrid experiment behind the `experimental-jit` Cargo feature. It recognizes and compiles the full Mandelbrot listing's outer raster loops and iteration math into one Cranelift frame kernel; ColourTrans and `OS_Plot` effects use checked runtime callbacks, while mode setup and the final key wait remain interpreted. The reduced Mandelbrot fixture uses its verified iteration kernel, and ClockSP5 uses its verified nested integer-repeat region. With default preferences `BASICRUN` remains the interpreter reference path; `BASICJIT` provides a one-run engine override and accepts a source or tokenized filename, so both input formats reach the same parser output and verified JIT regions. This remains sample-specific work: it does not establish the production IR or runtime ABI, compile either demo generically, or settle AOT packaging.

The experiment also compiles eligible scalar numeric statements from general ARM BASIC V programs, including assignments, numeric procedure arguments, conditions, and selected graphics arguments. It currently supports constants, numeric variables, unary signs, `+`, `-`, `*`, `^`, comparisons, and `ABS`, `COS`, `INT`, `LN`, `LOG`, `SIN`, `SQR`, and `TAN`; the resulting statements still pass through the interpreter's normal assignment, branch, procedure, or graphics path. It also compiles a conservative class of self-recursive numeric procedures whose first parameter counts down by one, whose zero case returns with `ENDPROC`, and whose body uses numeric scalar assignments, conditions, `GCOL`, `MOVE`, and `DRAW`. The procedure runs as one native call tree; global scalar reads and writes and graphics operations cross checked runtime callbacks. Each invocation is admitted only when its first argument is a nonnegative integer within the 64-level depth bound and its estimated call tree fits the one-million-entry native call budget; rejected invocations enter the interpreter, where recursive calls are considered individually. Unsupported expressions and statements stay interpreted. The source parser also accepts underscore-prefixed procedure/function names and leading-decimal numeric literals. The runtime supports the integer ARM-style `@%` controls for fixed and exponent formatting and restores its default on `@%=0`; complete field-width and formatting behavior remains open. The retained Agon fixture is `demo-volume/AgonTREE.bbc`, the repository's only Agon demo. It provides a recursive tree workload. Agon's VDP/GPIO services and full graphics dialect are not implemented in the current hosted profile; the planned Agon target profile above keeps those semantics and services distinct from RISC OS compatibility.

The strict native compiler now provides a separate whole-program path over `ParsedProgram`; the earlier `BASICJIT` experiment above remains hybrid and unchanged. Strict mode compiles every instruction and callable unit to Cranelift control flow before it starts the program. Arithmetic and loop updates run in generated code with stable scalar slots; an opaque runtime context provides checked services for strings, arrays, DATA, printing, clocks, input, task memory, CLI, and the existing MOS `CALL` dispatcher. Helpers receive values and logical guest addresses, never AST nodes or executable host pointers, and cannot unwind panics across the C ABI. A strict run never falls back: compile diagnostics include the BASIC line, and reached unsupported guarded operations and unknown machine-code addresses report explicit runtime errors. Native loop back-edges and call entry enforce the shared execution limit, call-depth bound, and periodic key polling. Reports expose statement and expression interpreter counts separately from helper calls; strict success requires both interpreter counts to be zero.

`BASICJIT STRICT [--benchmark-validation] [file]` runs this mode from the MOS prompt. With no file it uses the loaded tokenised program; a supplied `.bas`/`.bas64` source or `.bbc` file is compiled directly. `*CONFIGURE BASICEngine STRICT` makes strict mode the saved choice for `*BASIC`, `RUN`, `BASICRUN`, and desktop-launched BASIC programs; `BASICJIT` is still a one-run explicit override. `--benchmark-validation` disables Cranelift optimization for acceptance runs so ClockSP5's empty procedure calls and counting loops remain in the measured workload. This mode validates that lowering retains the work; its hosted MHz comparison is not evidence of a native speedup. ClockSP5's source and tokenised fixtures exercise three full passes through setup, all benchmark sections, and reporting, then return to the caller. The strict path currently covers the fixture and focused regression cases, not arbitrary BASIC V/VI or the unsupported graphics and `SYS` surface. With the default configuration, `BASICRUN` remains the behavioral interpreter reference.

**Exit:** interpreted and compiled execution share observable behavior for BASIC64 and each implemented classic compatibility profile; compiled artifacts can be tied to the source, compatibility profile, target, and runtime dependencies they were built from.

### Phase 7 — Portability and broader hardware ambition

Evaluate additional host systems, rendering backends, packaging, and possible dedicated hardware only after the hosted environment demonstrates the design. Bare-metal work is a separate architectural step, not a prerequisite for the Acorn-2026 experience.

**Exit:** platform-specific code remains behind stable runtime and rendering boundaries.

## 12. Open design questions for Codex

Work through these in order; preserve open questions rather than silently converting guesses into requirements.

1. **Compatibility baseline:** Which exact BBC BASIC V and VI versions, documented behaviors, extensions, and known quirks define source compatibility? Initial program-source candidates are MAL, WimpLib, and the Archimedes Notify archive, with Wimp acceptance deferred until its service surface exists; which of these become required acceptance targets, and which exact ROM saves anchor them?
2. **Shared tokenised format:** Which record-boundary layouts, token maps, and line-reference rules are used by the earlier BASIC versions? Which version-specific language and runtime behaviors must remain distinct after the shared decoder normalizes the file?
3. **Compatibility selection:** Which exact classic BASIC release profiles are supported, and how are profiles assigned to tokenised files that cannot carry a source directive? What default classic profile applies when the originating release is unknown?
4. **BASIC64 System Profile details:** The initial feature categories are settled in [`basic64-system-profile.md`](basic64-system-profile.md). What exact grammar, keyword/profile compatibility, integer suffix and overflow rules, string representation, record value semantics, structured-error mechanics, and manifest/annotation precedence should System Profile 0.1 use?
5. **SWI contract:** Which parts of the register/calling convention, errors, flag behavior, argument blocks, and module lifecycle must remain byte-for-byte compatible? The first icon-art extension uses a separately named, versioned `Wimp_CreateIconEx` SWI and leaves the standard `Wimp_CreateIcon` block untouched; should later extensions follow this pattern or use a broader extension registry?
6. **Pointer descriptors:** How are pointer-bearing SWI arguments described? What are the exact rules for buffers retained after return, callbacks, vectors, async I/O, and module-held references?
7. **Shared memory and Dynamic Areas:** What names or handles identify a shared region? Who can map it, resize it, revoke it, or free it? How are old Dynamic Area calls represented?
8. **Task and module lifetime:** Are modules reentrant? How is module workspace allocated per call or per task? What happens to retained references when a task or module exits?
9. **Address layout:** What logical ranges and alignment rules are visible in each personality? Which historical BASIC memory variables/operators must retain exact meaning?
10. **ARM compatibility scope:** Which ARM ISA generations and legacy modes are required? Is an interpreter sufficient initially, and what semantics must a translator preserve for memory and SWIs?
11. **Rendering backend — settled for the current desktop migration:** retain `winit`, use `wgpu` with Vello for host-scale composition, and use Parley with bundled Inter for modern UI text. Keep the existing CPU compatibility renderer authoritative for classic graphics until independent task/window surfaces preserve pixel reads and destination-dependent operations. Vello's experimental compute renderer currently has no software fallback; revisit that only if supported hosts or visual checks show a concrete blocker.
12. **Text profiles:** Which legacy metrics and layout behaviors can be reproduced? How does an application declare a profile, and how are profile changes prevented from breaking measurement/painting consistency?
13. **Desktop input:** How should Select/Menu/Adjust work on a trackpad or two-button mouse, and what accessibility remapping is needed?
14. **Application bundles:** What is the directory layout and manifest format? How are file types, launch behavior, permissions, updates, and resource lookup described while keeping the bundle inspectable?
15. **Trust boundary:** Which services are safe for every task, which require capabilities, and which components are trusted? How does BASIC64 system code receive privileges without giving every application access to system memory?
16. **First host target:** The initial host is macOS with a single in-window display. `--stdio` retains the terminal adapter. Which additional host systems and runtime backends should follow?
17. **Guest path syntax — first hosted volume settled:** `HostFS::DemoDisk`, dot-separated path elements, `$`, `@`, `^`, `&`, `%`, and `\`, and regular `.acornmeta`/`.acorn-volume` files define the initial folder-backed volume contract. Which escaping rules, search-path variables, cross-volume behavior, and additional FileSwitch conventions should be added next?
18. **Clean-code eligibility:** Which operations and effects disqualify code from the clean subset, at what granularity is eligibility established, and how are dynamic calls or changed dependencies revalidated? Which BASIC64 system components are good initial workloads for this analysis?
19. **Compilation units and artifacts:** Should the first native unit be a procedure, BASIC64 module, or whole file? Does the preferred Cranelift backend meet real JIT/AOT workload needs, and how should source, classic BASIC version/profile, dependencies, runtime ABI, target, and BASIC V library lookup affect compilation and cache invalidation?
20. **Agon target profile:** Which Agon BASIC variants and minimum MOS/VDP versions define the initial target? Which VDU, graphics, sound, input, MOS, and GPIO contracts are required, and how should unavailable hardware capabilities behave under the hosted adapter?

## 13. Working principles

- **Build the computer Acorn might have built in 2026.**
- **Preserve the contract, not obsolete implementation.**
- **Addresses are local. Services are global.**
- Keep documented compatibility stable; require a very good reason to change a public API.
- Keep the trusted Rust core focused on mechanisms; implement most user-facing OS policy in inspectable BASIC64.
- Make modern capabilities system-wide services so every application can benefit without selecting its own stack.
- Keep old and new execution personalities explicit when their semantics differ.
- Treat implementation details and unresolved choices as questions until evidence settles them.


### Filer directory viewers and modern folder art (2026-09-28)

BASIC64 owns a separate viewer for each visited directory, preserving its icons,
selection, display/sort mode, paging and Wimp scroll position. Double Select opens
a directory in another window and leaves its parent open; double Adjust opens it
and closes the source. Revisiting a directory raises/reopens its existing viewer
instead of duplicating it. Adjust-Close opens the parent and closes the child,
keeping the close-button position stable. Select on Open parent (and Backspace)
leaves the child open; Adjust on Open parent closes it. Ordinary Close closes only
that viewer, not the Filer task. Single Select selects; single Adjust toggles.
These rules follow the [RISC OS Filer guide](https://www.riscos.com/support/users/userguide3/bookb/book_3.html)
and [first steps guide](https://www.riscos.com/support/users/firststeps/chap07.htm).
Shift-modified application-directory/iconise variants are not yet implemented.
The initial table caches up to 64 directory viewers per Filer task; closed viewers
retain state until task exit. Paths remain bounded by the existing 256-byte HostFS
buffers. Window title bit 8 uses the standard indirect text descriptor, checked
against guest memory; title data is copied at creation, not retained as a host pointer.

Modern Filer directory icons use the cached `directory-glossy-v1-1024.png` master.
Transparent margins are trimmed and aspect ratio preserved within the existing
56-unit large-icon slot (or compact display slot), with high-quality Vello image
sampling at host display scale. The source PNG is unchanged. Compatibility sprite
rendering is retained for other guest applications.

Modern Filer selection highlights the filename while leaving the icon artwork
unchanged, including caller-supplied fallback artwork. The complete icon and label
remain the existing Wimp click target. Compatibility-profile icons retain their
selection recolouring; selection flags and public Wimp contracts are unchanged.
The compositor reuses converted icon images and modern text layouts across
repaints with bounded caches, keeping selection colour separate from text shaping.
Classic guest graphics remain CPU-authoritative; line rasterization holds the
surface lock for a complete primitive rather than acquiring it for every pixel.

### BASIC64 source filetype

Acorn-2026 assigns the local user-range filetype **&064 (BASIC64)** to UTF-8
BASIC64 source, distinct from tokenised BBC BASIC **&FFB** and plain Text **&FFF**.
This is a project-local convention, not a globally registered allocation;
[the RISC OS PRM](https://www.riscos.com/support/developers/prm/filetypes.html)
reserves &000–&0FF for users. Keeping it within 12 bits preserves the existing
OS_File/OS_FSControl catalogue and type fields without truncation or new SWIs.
`*FILETYPE <path> BASIC64` and `*FILETYPE <path> &064` are equivalent.

The checked-in `.bas64` sidecars, including System.Desktop and System.Filer,
use &064. Filer treats it as executable BASIC source and uses its dedicated BASIC64 PNG
via `file_064`; the compatibility renderer falls back to the classic BASIC sprite. RUN/BASIC retain their existing UTF-8
source loader for &064, and legacy source files marked Text remain loadable.
BASICLOAD remains specifically for tokenised &FFB files. Host extensions do not
override explicit metadata. Other machines need this local type association or
can retype exported source as Text; the payload itself stays ordinary UTF-8.

Filer keeps a work extent at least as large as the hosted 1600×1200 desktop
(and larger vertically when the catalogue needs it), rather than shrinking its
maximum size to the current icon grid. Large and small icon layouts use all
available columns. This permits enlarging a viewer again after shrinking while
preserving the Wimp's standard extent-bound resize constraints. Scrollbars describe
that workspace, including spare space below short catalogues.

New directory window definitions also start with the desktop-wide extent, before
their first OpenWindow call, so children can inherit an enlarged parent safely.

Classic guest console content uses the same bitmap/teletext renderer and palette
in Wimp output windows as fullscreen. Desktop composition must not substitute
outline fonts or recolour guest text; modern typography belongs to host furniture
and explicitly modern Wimp interfaces.

Raster clears convert inclusive GraphicsWindow corners into exclusive pixel
rectangle ends after pixel mapping. Adjacent Wimp redraw rectangles therefore
cover their shared edges without leaving stale strips during resize, including
in classic modes with multiple OS units per pixel.

Host-owned BASIC output windows commit move, resize, toggle and scroll requests
on the UI thread without guest polling or output. Their CPU raster and MODE
remain unchanged; resizing changes the viewport. Guest-created Wimp windows
retain the standard Open_Window_Request/Wimp_OpenWindow handshake.

### Optional RO 3.71 bevelled furniture

`*CONFIGURE WindowFurniture Bevelled` selects pinned RO 3.71 Tools3d button,
title and scrollbar artwork; `*CONFIGURE WindowFurniture Flat` restores the
existing vector furniture. `*STATUS WindowFurniture` reports the saved choice.
The option is read when the desktop renderer is created; restart the host app
after changing it. Existing configurations and DEFAULTS remain Flat. Geometry,
hit targets, file icons and modern title typography are unchanged; the active
title retains its yellow tint. Pattern strips tile rather than stretching across
long titles and scrollbars. This setting affects the Vello desktop; the legacy
software snapshot renderer remains flat.

This is an Acorn-2026 configuration name, not a claim to implement a historical
command. The [RISC OS PRM](https://www.riscos.com/support/developers/prm/wimp.html)
documents the 3D-look CMOS flag (byte 140 bit 0), while WimpFlags governs window
dragging and other behaviour. CMOS access for this preference is not implemented.

### OS icon Task menu and desktop prompts

Desktop.bas64 owns a two-entry Task menu on Menu-click of the far-right OS icon:
`*Commands` opens a MOS command console; `BASIC window` opens an isolated BASIC
source console. Acorn_Desktop action 4 registers the caller as the OS-icon Menu
recipient (Mouse_Click with window/icon -1). Wimp_StartTask additionally accepts
`Commands` and bare `BASIC`, represented as typed host launch requests. Neither
console reapplies the startup Language preference. Existing BASIC path launches
are unchanged. Menu is middle-click or Option-left-click on macOS; right-click is
Adjust. F12 shortcuts and unsupported original Task-menu rows are not implemented.

The BASIC prompt supports immediate source statements, numbered line entry or
replacement/deletion, LIST, RUN, NEW, star commands, and QUIT. Each immediate
submission or RUN uses the current source executor; variables are not retained
between separate immediate submissions. It is an initial source console, not a
complete BBC BASIC editor (LOAD/SAVE/EDIT are not implemented). A numbered program
executes as one source unit. Commands uses the existing MOS dispatcher, including
CONFIGURE, STATUS and BASIC file launching. Closing an idle console disconnects
its input so ReadLine exits; CPU-bound program cancellation remains separate.

Desktop console windows start with a visible work area matching the default
1280 × 1024 OS-unit guest surface. A guest MODE change updates only its host-owned
console extent and fits the visible area within the desktop; repeated frames do
not undo user resizes. Larger guest surfaces remain clipped at their native
integer pixel scale, with vertical scrolling (horizontal scroll furniture is not
yet implemented). Ordinary Wimp windows retain application-owned extents.
Extended MODE blocks publish their new raster immediately, just like numbered
VDU modes. Desktop file launches use the same throttled snapshot batching and
final-frame publication as MOS BASIC launches, so ColourTrans drawing and shared
true-colour surfaces reach the compositor without replaying millions of plots.

### Display Manager and hosted display settings

The Display Manager is BASIC64 desktop policy. Its monitor icon uses the supplied
`display-glossy-v1-1024.png` system artwork and opens one reusable window with
Colours and Resolution menus and Cancel/Change buttons. Menu choices are pending
until Change; Cancel discards them. Accepted settings are saved together through
the host configuration store. A save failure leaves the active settings unchanged.

Resolution describes the Acorn workspace, never the host monitor mode. Window
tracks the host client area's logical size, including subsequent resizes. Fixed
640×480, 800×600, 1024×768, 1152×864, 1280×1024 and 1600×1200 settings retain their
logical dimensions while the host window scales/letterboxes that workspace.
There are two Wimp OS units per logical desktop pixel. Host backing pixels are
separate: Vello renders at physical surface resolution while drawing and pointer
input share the same transform. The old 1600×1200-OS-unit default corresponds to
800×600 logical pixels; guest coordinate blocks remain expressed in OS units.
Window plus 16 million colours is the default for configurations without display
settings. Shared mutable metrics replace fixed runtime desktop bounds. Mode
changes retain guest surfaces and work extents, keep window controls reachable,
and notify affected guest windows so their content can reflow.

Colours constrains the final composed desktop on the GPU. The compositor and
source images remain full-colour: black/white and 4/16/256 greys use quantized
luminance, 16 and 256 colours use the bundled Wimp and `8desktop` palettes,
32 thousand uses RGB555, and 16 million preserves RGB888 output. Presentation
must retain the existing sRGB round-trip rules. These are output profiles, not
indexed guest framebuffer implementations: BASIC MODE, palette state and
synchronous OS_ReadPoint results are unchanged. Lower colour counts do not
promise better performance. Switching back restores the original source colours.

The additive named `Acorn_Display` service has a register-only versioned ABI:
R0=1, R1=0 queries; R0=1, R1=1 applies R2=resolution and R3=colour together.
Resolution IDs 0–6 are Window followed by the six fixed modes above. Colour IDs
0–7 are black/white, 4 greys, 16 greys, 16 colours, 256 greys, 256 colours,
32 thousand and 16 million. Query/apply returns active IDs in R2/R3, active logical
dimensions in R4/R5 and current host logical dimensions in R6/R7. R8 is zero on
success or one when saving an otherwise valid request fails. Invalid ABI versions,
actions and enum IDs are checked errors. The service does not expose host pointers
or reinterpret an existing RISC OS SWI.
