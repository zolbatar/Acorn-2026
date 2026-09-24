# Phase 0: MOS `*` Prompt Plan

## Purpose

Phase 0 defines the smallest reviewable contract for reaching a usable MOS command prompt. It precedes implementation and narrows the design brief's broader compatibility and service-catalog work to the interfaces required by this first milestone. The wider BBC BASIC V/VI and SWI compatibility work remains part of the project roadmap.

## First milestone

The hosted runtime starts, loads the BASIC64 boot path, and reaches a `*` prompt. The command loop accepts repeated commands, reports an unknown command and returns to the prompt, can enter the BASIC64 immediate prompt and return to MOS, and exits cleanly when requested.

The prompt and command behavior at the end of the milestone are implemented in inspectable BASIC64 source. A temporary prompt printed directly by Rust is acceptable as an earlier bring-up checkpoint.

## Phase 0 work

| Work item | Decision or artifact |
| --- | --- |
| Acceptance contract | Define the end-to-end demo: launch the hosted runtime, load the boot program, reach `*`, accept commands, handle an unknown command, enter BASIC64, return to MOS, and exit. |
| Host and boot path | Recommend macOS terminal as the first host. Choose where the BASIC64 boot source lives and define startup behavior for missing or invalid boot source. |
| Command contract | Select a small demo command set, for example `HELP`, `BASIC`, and `QUIT`. Record command casing, arguments, errors, and how control returns from BASIC64. Treat these names as prototype choices until compatibility review. |
| BASIC64 compatibility slice | List the syntax needed by the shell, likely string values, `PRINT`, input, conditionals, and loops. Mark each item as matching documented BBC BASIC V/VI behavior, an extension, or deferred. This is a bounded slice, not a claim of full compatibility. |
| Service catalog | Specify the Rust-to-BASIC64 boundary for terminal input/output, boot-source loading, and entering the interpreter. Identify which calls reuse public MOS/SWI contracts and which are internal bootstrap services. Check documented contracts before fixing names or behavior. |
| Runtime and memory boundary | Record the initial runtime as one interactive task with logical memory mediated by Rust. Mark modules, shared memory, multiple tasks, and retained pointers as later work. |
| Acceptance script | Write the exact manual steps and expected results so implementation can be checked against the same contract each time. |

## Recommended scope choices

- The MOS command loop and prompt-facing behavior live in BASIC64 source. Rust supplies terminal I/O, task context, memory mechanisms, and the service boundary.
- The final prompt is produced by the BASIC64 boot path. A temporary Rust prompt is useful as an early bring-up checkpoint.
- Use terminal line input for the first version. Custom line editing and desktop input can follow later.
- Keep the first service surface narrow and route it through the planned dispatcher so the shell does not gain a one-off Rust interface.
- Defer the Filer, general file commands, desktop, graphics, broad SWI coverage, ARM execution, and JIT until after the prompt milestone.

## Phase 0 exit criteria

Phase 0 is complete when these reviewable artifacts exist:

1. A prompt acceptance specification with a concrete manual demonstration sequence.
2. A prompt-specific compatibility matrix that labels required, extended, and deferred behavior.
3. A service catalog that labels each initial call as public-compatible or internal bootstrap and records its caller and data contract.
4. A brief runtime and memory boundary statement for the first single-task prototype.

Phase 1 can then implement the hosted runtime skeleton against the agreed behavior.
