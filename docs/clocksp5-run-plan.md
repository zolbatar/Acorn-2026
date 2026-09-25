# ClockSP5 execution plan

## Status: complete

The acceptance run is implemented. The checked-in tokenized fixture loads as 143 lines with 37 line references, prints all nine benchmark sections and the final comparison in each of its three workload passes, then returns to the MOS `*` prompt. `QUIT` remains available afterward. The hosted profile bypasses the guarded native ARM/hardware setup and treats the final `*FX151,78,243` reset command as a no-op. The runtime's MHz figures are a hosted-interpreter comparison against the fixture's BBC B reference data, not a physical host CPU clock measurement. This milestone implements only the constructs and guarded hosted-OS behavior reached by ClockSP5; general BASIC V/VI compatibility remains future work.

## Goal

Run the checked-in ARM BASIC V tokenised file with `BASICLOAD` and `BASICRUN`, through the compatibility executor, and have ClockSP5 print its benchmark sections and return to the MOS `*` prompt. Keep `ClockSP5.bas` and `ClockSP5.bbc` as the source and acceptance fixture. This is a program-driven compatibility milestone, not a claim of complete BBC BASIC V/VI support.

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

Each implementation milestone gets its own focused commit. Run the small echo fixture and Rust checks after interpreter changes, then use the ClockSP5 end-to-end run as the final acceptance check.

## Acceptance command

From the repository root:

```text
BASICLOAD examples/clocksp5/ClockSP5.bbc
BASICRUN
```

The program should print its benchmark headings and comparison output, then return to `*`. `QUIT` should still exit normally.
