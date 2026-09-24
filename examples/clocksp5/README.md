# ClockSP5 compatibility fixture

ClockSP5.bas is the text program from [dp111/ClockSP5](https://github.com/dp111/ClockSP5), version 5.08. The upstream repository identifies its license as GPL-3.0; see the [upstream license](https://github.com/dp111/ClockSP5/blob/main/LICENSE).

ClockSP5.bbc is a generated ARM BBC BASIC V tokenised saved-program file for testing the legacy file-format path. Its line records and encoded line references follow the [BBC BASIC V file format](https://xania.org/200711/bbc-basic-v-format). Regenerate it with `python3 tools/tokenize_clocksp5.py examples/clocksp5/ClockSP5.bas examples/clocksp5/ClockSP5.bbc`; the converter covers this fixture's tokens and source conventions, not general BBC BASIC input.

Run `cargo run`, then enter `BASICLOAD examples/clocksp5/ClockSP5.bbc` at the MOS prompt. The loader reports the 143 lines and 37 line references. `BASICRUN` currently supports only a small input/print/end subset, so ClockSP5 is a format fixture and is not executable yet.

The current Acorn-2026 runtime does not execute this program; it uses hardware and OS features outside the implemented slice.
