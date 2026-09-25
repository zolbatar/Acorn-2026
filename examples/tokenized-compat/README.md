# Tokenized BASIC compatibility corpus

`tdu-01-test.bbc` is copied byte-for-byte from the `$.TEST` BASIC file on the TDU-01 DFS disk image (`Tdu1.ssd`). The [8-Bit Software catalogue](https://8bs.com/catalogue/tdu.htm) says The Disk User cover discs were released to the public domain. The archive's original BBC BASIC ROM version is not recorded, so this fixture establishes a real BBC Micro saved-program input, not a precisely identified BASIC I/II/III/IV release.

The extracted file is 479 bytes and has SHA-256 `ef4ff46dbc13bbc67215e7d8876b8492a26c627a58ffede44a0f0aa202fdc1be`. It has four lines numbered 20, 150, 200, and 250. It is used only to test saved-program decoding; its machine-code calls are outside the current runtime.

The ClockSP5 and echo fixtures exercise the ARM-style layout and have project-maintained readable sources. ClockSP5's tokenized file is produced by `tools/tokenize_clocksp5.py`; it is not an independently saved binary from an ARM BASIC V installation. See [the compatibility matrix](../../docs/tokenized-basic-compatibility.md) for evidence and remaining coverage gaps.
