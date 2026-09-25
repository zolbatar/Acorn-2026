# Mandelbrot Cranelift feasibility benchmark

This isolated benchmark compiles the Mandelbrot listing's `PROCit` numeric loop
to native code with Cranelift, runs the same 80 × 50 sample through the hosted
ARM BASIC V compatibility interpreter, and also emits a native object file.
The sample uses the original centre, scale, aspect ratio, and iteration cap but
omits display setup, color conversion, and plotting. The interpreter timing
covers the full tokenized BASIC grid program. The native timing covers a Rust
grid loop calling the Cranelift-compiled iteration kernel, so this measures a
useful JIT opportunity rather than a complete compiled BASIC program. BASIC
tokenization and native code compilation are reported separately from execution.
This is a backend feasibility check, not the BASIC64 compiler or a general BASIC
JIT.

The small Cargo project pins Cranelift separately from the application. This
keeps experimental compiler dependencies out of the runtime's normal build.

Regenerate the interpreter fixture and run the benchmark from the repository
root:

```sh
python3 tools/tokenize_mandelbrot_benchmark.py
cargo run --release --manifest-path tools/mandelbrot-jit-bench/Cargo.toml
```

The program checks that the interpreted and JIT iteration checksums agree. It
reports median times from three interpreter runs and seven native runs, JIT
compilation time, AOT object compilation time, and the generated object path
and size. The object file is written under the repository `target/` directory.
Run this on the target Apple Silicon Mac for the intended host measurements;
timings from other hosts are still useful as a sanity check, but are not the
target result.
