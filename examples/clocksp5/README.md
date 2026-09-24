# ClockSP5 compatibility fixture

ClockSP5.bas is the text program from [dp111/ClockSP5](https://github.com/dp111/ClockSP5), version 5.08. The upstream repository identifies its license as GPL-3.0; see the [upstream license](https://github.com/dp111/ClockSP5/blob/main/LICENSE).

ClockSP5.bbc is a generated ARM BBC BASIC V tokenised saved-program file for testing the legacy file-format path. Its line records and encoded line references follow the [BBC BASIC V file format](https://xania.org/200711/bbc-basic-v-format). Regenerate it with `python3 tools/tokenize_clocksp5.py examples/clocksp5/ClockSP5.bas examples/clocksp5/ClockSP5.bbc`; the converter covers this fixture's tokens and source conventions, not general BBC BASIC input.

The current Acorn-2026 runtime does not yet execute this program; it uses hardware and OS features outside the implemented slice.
