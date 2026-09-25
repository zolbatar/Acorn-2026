# Phase 0: MOS `*` Prompt Plan

## Purpose

Define a small, reviewable contract for the first hosted prototype: a Rust runtime that reaches the MOS `*` prompt and accepts `HELP` and `QUIT`. The initial implementation used a terminal command line; the default frontend has since advanced to a single graphics-capable window, while `cargo run -- --stdio` retains the original terminal adapter. BASIC64 was deferred until after the prompt bring-up milestone. The long-term architecture in the design brief remains the project direction.

## First milestone

The hosted Rust executable starts one command task, shows `*`, reads a line through `OS_ReadLine`, and sends the command through `OS_CLI`. The built-in commands are `HELP` and `QUIT`. `HELP` prints the command list through the output SWIs; `QUIT` ends the runtime without printing another prompt. An unrecognized command reports `Bad command` and returns to the prompt. End-of-input also exits the hosted process. In windowed mode keyboard input is captured by the window and visible output is rendered from the same SWI-driven display state.

The user types `HELP` after the `*` prompt; the asterisk is the prompt marker and is not part of the command text.

**Acceptance check (2026-09-24):** The user confirmed that `HELP` displays the help text and returns to `*`, and that `QUIT` exits the runtime.

## Phase 0 work

| Work item | Decision or artifact |
| --- | --- |
| Acceptance contract | Define the end-to-end demo: open/run the Cargo project, see `*`, type `HELP`, see help text and return to `*`, then type `QUIT` and confirm clean exit. Record unknown-command and end-of-input behavior. |
| Host and project | Use a hosted terminal on macOS for the first target. Keep host I/O behind a Rust adapter and make the repository root a Cargo project that RustRover can open. |
| Command contract | Implement `HELP` and `QUIT` as the first built-in MOS commands. Match them case-insensitively; `QUIT` requests a clean runtime exit and other input gets `Bad command`. |
| SWI catalog | Define the initial subset: `OS_WriteC`, `OS_WriteS`, `OS_Write0`, `OS_NewLine`, `OS_ReadC`, `OS_ReadLine`, and `OS_CLI`. Keep their numeric IDs and documented register/memory behavior. |
| Caller and memory boundary | Carry a caller task and its logical address space into each SWI. Pointer arguments resolve through checked guest-memory access; no host pointers enter the guest interface. |
| Compatibility boundary | Preserve documented behavior for the initial SWIs where feasible. Mark terminal-specific limitations and all unimplemented SWIs as outside this prototype slice. BASIC64 semantics and compatibility are deferred. |
| Acceptance script | `cargo run`, enter `HELP`, confirm help output and the next `*` prompt, then enter `QUIT` and confirm the runtime exits without another prompt. Also record an unknown-command check. |

## Initial SWI surface

The first dispatcher keeps the documented RISC OS SWI numbers and the core register/data behavior:

| SWI | Number | First-prototype behavior |
| --- | --- | --- |
| `OS_WriteC` | `&00` | Write the low byte of R0. |
| `OS_WriteS` | `&01` | Write the null-terminated inline string at the caller's saved return address, then advance the return address to the next word after the string. |
| `OS_Write0` | `&02` | Write the null-terminated string addressed by R0; return R0 pointing after its terminator. |
| `OS_NewLine` | `&03` | Write line feed followed by carriage return. |
| `OS_ReadC` | `&04` | Read one input byte to R0; set carry for Escape. |
| `OS_CLI` | `&05` | Execute the null-terminated command addressed by R0; preserve R0. |
| `OS_ReadLine` | `&0E` | Read into the logical buffer addressed by R0, limited by R1 and the ASCII range in R2-R3; return the character count in R1 and set carry for Escape. |

SWI pointer arguments are logical addresses scoped to the calling task. The dispatcher validates each access against that task's guest memory. The host console remains a byte source/sink behind these handlers.

## Recommended scope choices

- The command loop is Rust for this bootstrap stage. BASIC64 comes later.
- Route visible console input/output through the SWI dispatcher. The host adapter supplies bytes and receives output bytes; it does not print the prompt or help text directly.
- `OS_WriteS` reads a null-terminated inline string from the caller's logical memory at the saved return address and advances that address past the string. `OS_WriteC`, `OS_Write0`, and `OS_NewLine` use the corresponding documented behavior.
- The shell reads command lines with `OS_ReadLine`, then invokes `OS_CLI` with a pointer to the null-terminated command in the task's logical memory.
- Start with one caller task and only the logical memory needed by pointer-bearing SWIs. Full task isolation, modules, shared memory, and the broader SWI set remain later work.
- Use the host terminal's line discipline when available. The initial command line does not require desktop input, custom editing UI, or BASIC64.

## Phase 0 exit criteria

Phase 0 is complete when these artifacts are reviewable:

1. A concise prompt acceptance contract and manual demonstration sequence.
2. A catalog for the initial console and command-line SWIs, including entry/exit registers and pointer semantics.
3. A caller/memory boundary note for the one-task prototype.
4. A RustRover-ready Cargo project plan with `HELP` and `QUIT` as its first commands.

## References

- [RISC OS PRM: Character Output](https://www.riscos.com/support/developers/prm/charoutput.html)
- [RISC OS PRM: Character Input](https://www.riscos.com/support/developers/prm/charinput.html)
- [RISC OS PRM: The CLI](https://www.riscos.com/support/developers/prm/cli.html)
- [RISC OS PRM: `*` Commands and the CLI](https://www.riscos.com/support/developers/prm/cliintro.html)
