# ClockSP5 execution plan

## Goal

Run the checked-in ARM BASIC V tokenised file with `BASICLOAD` and `BASICRUN`, through the compatibility executor, and have ClockSP5 print its benchmark sections and return to the MOS `*` prompt. Keep `ClockSP5.bas` and `ClockSP5.bbc` as the source and acceptance fixture. This is a program-driven compatibility milestone, not a claim of complete BBC BASIC V/VI support.

The saved-program decoder remains shared across earlier BASIC versions. The execution subset is being added against ClockSP5's token stream, using the existing line-reference decoder and caller-scoped logical memory. BASIC64 parsing remains independent.

## Source audit

ClockSP5 uses compact colon-separated tokenised lines; numeric, integer and string variables; arithmetic and comparison expressions; arrays; `DATA`/`READ`/`RESTORE`; `IF`; `GOTO`; nested `REPEAT`/`UNTIL` and `FOR`/`NEXT`; `PROC`/`ENDPROC`; `GOSUB`/`RETURN`; numeric and string built-ins; and the `TIME` and `INKEY` pseudo-variables.

The checked-in program starts with `Z%=&0211`. With the hosted no-key result (`INKEY` returns -256), its own branch sets `Y%=2`. That selects the BBC B BASIC II comparison data and avoids its processor-memory probe. The set low bit in `Z%` also makes `PROC s` return before the `CALL &FFF1`, memory write, and `*FX` setup lines. The demo therefore needs an accurate, advancing BASIC `TIME` value and a no-key `INKEY`, but not host-pointer access, native ARM execution, or hardware/OS emulation for these guarded paths.

## Implementation sequence

1. **Tokenized execution core:** replace the sequential statement-only runner with a token-aware execution cursor, scalar numeric/string values, expression evaluation, assignments, and checked branch targets. Keep console input/output on the existing SWIs.
2. **Program control and data:** support the control flow used in the fixture (`IF`, `GOTO`, nested `REPEAT`/`UNTIL`, `FOR`/`NEXT`, procedures, `GOSUB`/`RETURN`) and its `DATA`/`READ`/`RESTORE` tables. Enforce a bounded instruction count so a broken loop returns an error instead of hanging the MOS prompt.
3. **ClockSP5 language surface:** add the arrays, print formatting, numeric/string built-ins, and variable behavior reached by the fixture. Unsupported syntax should identify its BASIC line and token.
4. **Hosted OS profile:** expose `TIME` as monotonic centiseconds and `INKEY` as the BBC no-key result. Preserve checked task memory for any supported `?` or `!` access; leave the source's guarded native `CALL` and `*FX` path unsupported and report it if reached.
5. **Acceptance and documentation:** run the actual `.bbc` fixture from the MOS prompt, confirm each benchmark heading and the final comparison label appear, confirm it returns to `*`, then record the implemented subset and remaining compatibility gaps.

Each implementation milestone gets its own focused commit. Run the small echo fixture and Rust checks after interpreter changes, then use the ClockSP5 end-to-end run as the final acceptance check.

## Acceptance command

From the repository root:

```text
BASICLOAD examples/clocksp5/ClockSP5.bbc
BASICRUN
```

The program should print its benchmark headings and comparison output, then return to `*`. `QUIT` should still exit normally.
