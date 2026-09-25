# Acorn-2026

> **Build the computer Acorn might have built in 2026.**

Acorn-2026 is a design and implementation project for a modern, tinkerable computer environment inspired by Acorn and RISC OS. It preserves useful ideas and stable interfaces while replacing historical implementation limits. It is not a RISC OS simulator or a retro desktop remake.

## Project status

The hosted Rust MOS prompt milestone is verified: `HELP` displays help and returns to `*`, and `QUIT` exits. The first BASIC64 source slice runs `INPUT` and `PRINT` string variables from plain UTF-8 `.bas64` files using a pest grammar. `BASICLOAD` accepts two observed tokenized saved-program record layouts and preserves their token bytes. `BASICRUN` supports the string echo fixture and the ClockSP5 program-version-5.08 compatibility slice. ClockSP5 completes its benchmark sections and returns to `*`; this does not establish broad BASIC V/VI compatibility. See the [tokenized BASIC compatibility matrix](docs/tokenized-basic-compatibility.md) for evidence and gaps.

## Start here

- [`docs/acorn-2026-design.md`](docs/acorn-2026-design.md) — architecture, compatibility goals, memory model, SWIs and modules, rendering, desktop model, roadmap, and open questions.
- [`docs/phase-0-mos-prompt-plan.md`](docs/phase-0-mos-prompt-plan.md) — the saved Phase 0 prompt contract and initial SWI catalog.
- [`docs/clocksp5-run-plan.md`](docs/clocksp5-run-plan.md) — implementation sequence and acceptance criteria for running the ClockSP5 fixture.
- [`docs/tokenized-basic-compatibility.md`](docs/tokenized-basic-compatibility.md) — decoder, token-profile, and execution coverage by saved-program evidence.

## Current direction

- A hosted Rust runtime provides low-level, kernel-like services; this does not begin as a bare-metal kernel.
- BASIC64 runs in Rust, with an interpreter first and a JIT considered later.
- BBC BASIC V/VI source semantics are a compatibility target where feasible.
- SWI names and documented calling behavior are treated as stable public contracts.
- Services are global while task address spaces are logical and isolated.
- The desktop, Filer, Wimp-like behavior, modules, and most OS policy are intended to be inspectable BASIC64 code.
- The system owns modern graphics and text shaping through host rendering facilities.

## First design work

Use the open questions in the architecture brief to settle the compatibility baseline, SWI contract, task memory model, and later host targets. Keep those decisions explicit before they become implementation assumptions.

## Run the first hosted prompt

Open the repository root (the folder containing `Cargo.toml`) in RustRover, then run:

```sh
cargo run
```

At the `*` prompt, enter `HELP` to list the built-in commands. Enter `QUIT` to exit the runtime. End-of-input also exits the process.

To run the string echo example, enter:

```text
RUN examples/echo.bas64
```

The program asks for a line with `? ` and prints the entered string. Native BASIC64 source is currently plain UTF-8 identified by the `.bas64` extension.

To run the small tokenised BASIC echo fixture, enter:

```text
BASICLOAD examples/basicv-echo/echo.bbc
BASICRUN
```

Type a line when the program shows `? `; it prints that line back and returns to the MOS prompt.

To run ClockSP5, enter:

```text
BASICLOAD examples/clocksp5/ClockSP5.bbc
BASICRUN
```

It runs three workload passes, printing the benchmark sections and comparison each time, then returns to `*`. In this hosted profile, `TIME` is monotonic centiseconds and `INKEY` returns the no-key value, so the program follows its own guarded path that skips native ARM and hardware setup. Its final hardware reset command is accepted as a no-op. The displayed MHz figures compare this hosted BASIC interpreter's performance against the program's BBC B reference data; they do not measure the host processor's physical clock speed.
