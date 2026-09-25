# BASIC Cranelift feasibility benchmarks

This isolated Cargo project benchmarks selected BASIC compatibility workloads
against Cranelift-generated native code. The ClockSP5 pass takes an AST from the
hosted compatibility parser, lowers its supported integer subset into a small
backend-neutral IR, and then lowers that IR into Cranelift. The Mandelbrot pass
still uses hand-built Cranelift IR. Neither pass is a general BASIC compiler.

The Cranelift version is pinned separately from the application so these
experiments do not add compiler dependencies to the runtime build.

## Mandelbrot

The Mandelbrot benchmark compiles the listing's `PROCit` numerical iteration
loop and runs an 80 × 50 sample through the hosted ARM BASIC V compatibility
interpreter. The fixture keeps the listing's original centre, scale, aspect
ratio, and iteration cap while omitting display setup, color conversion, and
plotting. Interpreter timing covers the full tokenized BASIC grid program;
native timing covers a Rust grid loop calling the Cranelift kernel. It is a
useful feasibility comparison, not a complete compiled BASIC program.

Regenerate the fixture and run from the repository root:

```sh
python3 tools/tokenize_mandelbrot_benchmark.py
cargo run --release --manifest-path tools/basic-jit-bench/Cargo.toml --bin mandelbrot-jit-bench
```

The benchmark checks that the interpreted and JIT iteration checksums agree,
reports three interpreter and seven native samples, and emits an AOT native
object under `target/basic-jit-bench/`.

## ClockSP5 integer REPEAT section

The ClockSP5 pass isolates the integer REPEAT loop from lines 120–123 of
[`ClockSP5.bas`](../../examples/clocksp5/ClockSP5.bas). A project-authored
fixture fixes the upper bounds so the same deterministic loop can run in the
interpreter and in Cranelift. The compatibility parser produces a syntax AST;
`basic_compat::compiler_api::lower_integer_program` converts that AST to a
typed integer IR with source line numbers; the benchmark's Cranelift backend
converts the IR to JIT code and an AOT object. It preserves the nested
increment and post-tested REPEAT/UNTIL behavior, then returns the printed
integer variable as the compiled function result for checksum comparison.
This targets one section of the ClockSP5 suite; it does not compile the whole
program or its other real, string, procedure, GOSUB, and trig/log sections.

The prototype rejects unsupported statements and expressions explicitly. Its
integer operations preserve the interpreter's signed 32-bit conversion at
assignment boundaries, including saturation for addition results outside the
signed range.

Regenerate the fixture and run from the repository root:

```sh
python3 tools/tokenize_clocksp5.py examples/clocksp5/integer-repeat-jit.bas examples/clocksp5/integer-repeat-jit.bbc
cargo run --release --manifest-path tools/basic-jit-bench/Cargo.toml --bin clocksp5-jit-bench
```

The benchmark checks matching checksums, reports three interpreter and seven
native samples, and emits an AOT object for the kernel under
`target/basic-jit-bench/`. The full ClockSP5 acceptance run remains documented
in [`docs/clocksp5-run-plan.md`](../../docs/clocksp5-run-plan.md).

Run the benchmarks on the target Apple Silicon Mac for intended host
measurements. Reported speedups compare these specific workloads and harnesses;
they are not estimates for an end-to-end BASIC compiler.
