# Acorn-2026

> **Build the computer Acorn might have built in 2026.**

Acorn-2026 is a design and implementation project for a modern, tinkerable computer environment inspired by Acorn and RISC OS. It preserves useful ideas and stable interfaces while replacing historical implementation limits. It is not a RISC OS simulator or a retro desktop remake.

## Project status

The project is in the design phase. The architecture brief is the current starting point; it distinguishes intended principles from exploratory ideas and unresolved decisions.

## Start here

- [`docs/acorn-2026-design.md`](docs/acorn-2026-design.md) — architecture, compatibility goals, memory model, SWIs and modules, rendering, desktop model, roadmap, and open questions.

## Current direction

- A hosted Rust runtime provides low-level, kernel-like services; this does not begin as a bare-metal kernel.
- BASIC64 runs in Rust, with an interpreter first and a JIT considered later.
- BBC BASIC V/VI source semantics are a compatibility target where feasible.
- SWI names and documented calling behavior are treated as stable public contracts.
- Services are global while task address spaces are logical and isolated.
- The desktop, Filer, Wimp-like behavior, modules, and most OS policy are intended to be inspectable BASIC64 code.
- The system owns modern graphics and text shaping through host rendering facilities.

## First design work

Use the open questions in the architecture brief to settle the exact compatibility baseline, SWI contract, task memory model, and first hosted prototype. Keep those decisions explicit before they become implementation assumptions.
