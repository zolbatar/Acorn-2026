#!/usr/bin/env python3
"""Generate the tokenized compatibility fixture for the reduced Mandelbrot."""

from pathlib import Path

from tokenize_clocksp5 import encode_program


ROOT = Path(__file__).resolve().parents[1]
SOURCE = ROOT / "examples/mandelbrot/reduced.bas"
OUTPUT = ROOT / "examples/mandelbrot/reduced.bbc"


def main() -> None:
    encoded, line_count = encode_program(SOURCE.read_text(encoding="utf-8"))
    OUTPUT.write_bytes(encoded)
    print(f"wrote {len(encoded)} bytes across {line_count} lines to {OUTPUT}")


if __name__ == "__main__":
    main()
