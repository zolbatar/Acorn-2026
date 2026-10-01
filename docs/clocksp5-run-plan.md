# ClockSP5 execution plan

## Interpreter acceptance: complete; strict native acceptance: added

The interpreter acceptance run remains available: the checked-in tokenised fixture loads as 143 lines with 37 line references, prints all nine benchmark sections and the final comparison in each of its three workload passes, then returns to the MOS `*` prompt. The strict native path now accepts both checked-in source and tokenised fixtures through whole-program compilation, with zero interpreter statement dispatches and expression evaluations. Its tests check all three passes and every section before returning to the caller. The hosted profile bypasses ClockSP5's guarded native ARM/hardware setup and treats the final `*FX151,78,243` reset command as a no-op. The program's MHz figures compare hosted execution with fixture BBC B reference data; they are not a physical host CPU clock measurement or a strict-JIT speedup claim. General BASIC V/VI compatibility remains future work.

## Goal

Run the checked-in ARM BASIC V source or tokenised file directly with `*BASIC <file>`, through the configured compatibility executor, and have ClockSP5 print its benchmark sections and return to the MOS `*` prompt. Keep `ClockSP5.bas` and `ClockSP5.bbc` as the source and acceptance fixture. This is a program-driven compatibility milestone, not a claim of complete BBC BASIC V/VI support.

The additional strict-JIT goal is to compile either fixture completely before execution, preserve its measured calls and loops, run every pass without interpreter fallback, and report zero interpreted statements and expressions.

The saved-program decoder remains shared across earlier BASIC versions. The execution subset is being added against ClockSP5's token stream, using the existing line-reference decoder and caller-scoped logical memory. BASIC64 parsing remains independent.

## Source audit

ClockSP5 uses compact colon-separated tokenised lines; numeric, integer and string variables; arithmetic and comparison expressions; arrays; `DATA`/`READ`/`RESTORE`; `IF`; `GOTO`; nested `REPEAT`/`UNTIL` and `FOR`/`NEXT`; `PROC`/`ENDPROC`; `GOSUB`/`RETURN`; numeric and string built-ins; and the `TIME` and `INKEY` pseudo-variables.

The checked-in program starts with `Z%=&0211`. With no host key pending, bare `INKEY` returns -256 and its own branch sets `Y%=2`. That selects the BBC B BASIC II comparison data and avoids its processor-memory probe. The set low bit in `Z%` also makes `PROC s` return before the `CALL &FFF1`, memory write, and `*FX` setup lines. The demo therefore needs an accurate, advancing BASIC `TIME` value and a no-key result when the input queue is empty, but not host-pointer access, native ARM execution, or hardware/OS emulation for these guarded paths.

## Implementation sequence

1. **Tokenized execution core:** implemented a token-aware execution cursor, scalar numeric/string values, expression evaluation, assignments, and checked branch targets. Console input/output uses the existing SWIs.
2. **Program control and data:** implemented the fixture's `IF`, `GOTO`, nested `REPEAT`/`UNTIL`, `FOR`/`NEXT`, procedures, `GOSUB`/`RETURN`, and `DATA`/`READ`/`RESTORE` paths. Execution has a bounded instruction count.
3. **ClockSP5 language surface:** implemented the arrays, print formatting, numeric/string built-ins, and variable behavior reached by the fixture, with line-aware errors for unsupported input.
4. **Hosted OS profile:** `TIME` is monotonic centiseconds and `INKEY` returns the BBC no-key result. Checked task memory remains in place. ClockSP5's guards bypass its native `CALL` and hardware/OS setup paths; the final `*FX151,78,243` reset is accepted as a no-op.
5. **Acceptance and documentation:** the actual `.bbc` fixture ran from the MOS prompt, printed its benchmark output, and returned to `*`.

## Strict native execution

The Strict engine compiles the full shared `ParsedProgram` into native control flow before running it. Generated code performs numeric expressions, branches, loop arithmetic, and calls directly; checked runtime helpers handle dynamic strings and arrays, DATA, output, clocks, input, logical task memory, star commands, and MOS `CALL`. The helper ABI accepts values and an opaque runtime context. It does not dispatch BASIC statements or evaluate BASIC expressions. Strict success reports separate interpreter statement and expression counts, both zero; legitimate service-helper calls are reported separately. Unsupported compilation returns a BASIC line and reason before execution. A guarded supported AST operation without a native implementation emits an explicit error only if reached. Strict does not fall back to the interpreter or Hybrid JIT.

Use the internal benchmark-validation option for coverage runs. It turns Cranelift optimization off so empty `PROC` calls and counting loops remain present; do not use these timings to claim performance improvement. It is not exposed as MOS syntax. Normal Strict runs use Cranelift speed optimization. Configure `BASICEngine Hybrid` for the separate Hybrid engine used for Mandelbrot and recursive-procedure support, or `BASICEngine Strict` for Strict; then invoke either with `BASIC <file>` or `RUN <file>`. `REM @BASIC64` fields continue to override saved language, target, and profile preferences for the fields they declare. The retired load/cache and one-shot JIT command family has no compatibility alias; tokenized `.bbc` files are passed directly to `BASIC`/`RUN`.

Each implementation milestone gets its own focused commit. Run the small echo fixture and Rust checks after interpreter changes, then use the ClockSP5 end-to-end run as the final acceptance check.

## Acceptance commands

From the repository root:

```text
CONFIGURE BASICEngine Strict
BASIC examples/clocksp5/ClockSP5.bas
BASIC examples/clocksp5/ClockSP5.bbc
```

Each acceptance run should print all nine benchmark headings and the final comparison three times, return to the MOS prompt, and report zero interpreter statement and expression counts. Timing-dependent output is variable. With default configuration, `BASICEngine Interpreter` remains the behavioral reference. The benchmark-validation setting is exercised by Rust acceptance tests rather than the public MOS surface. `QUIT` should still exit normally.

Focused strict-mode regression cases are in `tests/basic_jit_strict.rs`. The MOS service case exercises OSWORD clocks, checked logical memory, and `CALL &FFF1` directly because ClockSP5's normal guard skips that path.
