# LearnAgon BASIC examples

This directory contains 32 `.BAS` programs copied from [learnagon/bbc-basic-by-example](https://github.com/learnagon/bbc-basic-by-example) at commit `548f41c90f2272ff7d8c7d985f385d7e70de3f74`. The upstream repository is released under CC0 1.0; its `LICENSE` is included here. `VW.BAS` uses the companion `vw.seq`, also retained from upstream. The three direct GPIO demos (`GPIO_CARD_LA_2`, `GPIO`, and `GPIO_TEST`) were removed because they require Agon Light GPIO hardware.

Each source file has a matching `.bbc` tokenised file. The `.bbc` files were made with [Steve Fryatt's Tokenize](https://github.com/steve-fryatt/tokenize) at commit `d5ea2f424bb7bcc9645416986b51f2ffb9b0e220`. The conversion script keeps the token and line-reference bytes, while changing the tokenizer's shared-CR framing to the separate-CR framing used by this repository's generated ARM-profile fixtures. These are converter-generated ARM-profile files; they are not saves produced by a named BBC BASIC V ROM. Tokenize does not check whether every statement is supported by this runtime.

Every source begins with `REM @BASIC64 MODE=CLASSIC TARGET=AGON`. This records the intended classic BASIC compatibility mode and Agon target so the examples are not mistaken for native BASIC64 programs. It remains a BASIC comment; the current runtime does not read it to select a profile. The paired `.bbc` file stores it as tokenised line 0.

Regenerate the `.bbc` files from the included sources with Tokenize available on `PATH`:

```sh
python3 tools/tokenize_external_basic_examples.py
```

Or select a Tokenize executable explicitly:

```sh
BBC_BASIC_TOKENIZER=/path/to/tokenize python3 tools/tokenize_external_basic_examples.py
```

Run a file through the experimental hybrid JIT from the MOS prompt:

```text
BASICLOAD examples/bbc-basic-by-example/operators-and-special-symbols/EXPONENTIATION-OPERATOR.bbc
BASICJIT
```

`BASICJIT` compiles eligible scalar numeric statements to Cranelift and executes other work through the compatibility interpreter. The exponentiation example and `interesting_programs/TIME_TEST.bbc` exercise native numeric statements. `SPLIT_EXAMPLE.bbc` exercises the JIT command's interpreter fallback for a program without a matching native region.

These programs target Agon Light BASIC. The set widens the source syntax and numeric JIT workload, but does not by itself define BBC BASIC V behavior: Agon modes, VDP access, and Agon-specific graphics/VDU commands require their own hosted services or a compatibility decision. The unmodified `TREE.bbc` requests Agon mode 20, while the hosted graphics profile assigns mode 20 its standard RISC OS meaning (640 × 512, 16 colours), not Agon's 512 × 384, 64-colour mode. The parser handles underscore-prefixed routine names and leading-decimal literals, and the runtime handles the integer ARM-style `@%` fixed/exponent controls used by the listing. Field-width and complete print-format behavior remain unsupported. Platform-specific examples remain in the corpus for future coverage and should not be counted as end-to-end runnable until those services are implemented.

The unmodified tree listing can also be run through the hosted compatibility profile:

```text
BASICLOAD examples/bbc-basic-by-example/TREE.bbc
BASICJIT
```

For supported nonnegative integer starting depths within the JIT's recursion and call budget, `BASICJIT` compiles the recursive `_DRAWTREE` procedure, including its numeric expressions and branches. `GCOL`, `MOVE`, and `DRAW` use checked runtime callbacks. A call outside those bounds enters the interpreter, where eligible recursive child calls are still considered individually. The graphics still follow the hosted RISC OS mode-20 profile, not Agon mode 20; this is a JIT coverage example rather than an Agon display compatibility claim.
