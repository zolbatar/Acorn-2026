# ClockSP5 compatibility fixture

ClockSP5.bas is the text program from [dp111/ClockSP5](https://github.com/dp111/ClockSP5), version 5.08. The upstream repository identifies its license as GPL-3.0; see the [upstream license](https://github.com/dp111/ClockSP5/blob/main/LICENSE).

ClockSP5.bbc is a generated ARM BBC BASIC V tokenised saved-program file for testing the legacy file-format path. Its line records and encoded line references follow the [BBC BASIC V file format](https://xania.org/200711/bbc-basic-v-format). Regenerate it with `python3 tools/tokenize_clocksp5.py examples/clocksp5/ClockSP5.bas examples/clocksp5/ClockSP5.bbc`; the converter covers this fixture's tokens and source conventions, not general BBC BASIC input.

Run `cargo run`, then enter these commands at the MOS prompt:

```text
BASIC $.clocksp5.ClockSP5
```

Both `.bbc` fixtures from this folder are staged under `demo-volume/clocksp5` with BASIC file type metadata and extensionless guest names (`ClockSP5` and `integer-repeat-jit`).

The loader reports 143 lines and 37 line references. The compatibility runner executes the subset used by this program and prints three complete workload passes, with the benchmark sections and comparison in each, before returning to `*`. Its hosted profile uses monotonic centisecond `TIME` and the no-key `INKEY` result. ClockSP5's own guards then skip its native ARM call and hardware/OS setup; the final hardware reset command is accepted as a no-op.

The MHz figures compare the hosted BASIC interpreter's elapsed loop work with the program's BBC B reference data; they do not measure the host processor's physical clock. This fixture-specific execution does not imply general BASIC V/VI compatibility. The shared tokenized file decoder is intended to serve files from all earlier BASIC versions, with their execution semantics being added separately.
