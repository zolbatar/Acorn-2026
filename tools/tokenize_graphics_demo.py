#!/usr/bin/env python3
"""Encode the tiny text-and-pixels compatibility fixture as a BBC V file.

This fixture-only encoder covers MODE, VDU, PRINT, GCOL, PLOT, and END. It is
not a general BASIC tokenizer or source validator.
"""

from __future__ import annotations

import re
import sys
from pathlib import Path


TOKENS = {
    "GCOL": 0xE6,
    "MODE": 0xEB,
    "PLOT": 0xF0,
    "PRINT": 0xF1,
    "VDU": 0xEF,
    "END": 0xE0,
}
TOKEN_NAMES = sorted(TOKENS, key=len, reverse=True)


def tokenise_line(text: str) -> bytes:
    output = bytearray()
    index = 0
    quoted = False
    while index < len(text):
        character = text[index]
        if character == '"':
            quoted = not quoted
            output.append(ord(character))
            index += 1
            continue
        if quoted:
            output.append(ord(character))
            index += 1
            continue

        name = next(
            (
                token
                for token in TOKEN_NAMES
                if text[index : index + len(token)].upper() == token
            ),
            None,
        )
        if name is None:
            output.append(ord(character))
            index += 1
        else:
            output.append(TOKENS[name])
            index += len(name)
    return bytes(output)


def encode_program(source: str) -> bytes:
    output = bytearray()
    for raw_line in source.splitlines():
        match = re.match(r"^\s*(\d+)\s+(.*)$", raw_line)
        if match is None:
            if raw_line.strip():
                raise ValueError(f"expected a numbered BASIC line: {raw_line!r}")
            continue

        line_number = int(match.group(1))
        if not 0 <= line_number <= 0xFEFF:
            raise ValueError(f"line number out of BASIC V range: {line_number}")
        body = tokenise_line(match.group(2))
        record_length = len(body) + 5
        if record_length > 255:
            raise ValueError(f"line {line_number} is too long for BASIC V")

        output.extend((0x0D, line_number >> 8, line_number & 0xFF, record_length))
        output.extend(body)
        output.append(0x0D)

    output.extend((0x0D, 0xFF))
    return bytes(output)


def main() -> None:
    if len(sys.argv) != 3:
        raise SystemExit("usage: tokenize_graphics_demo.py INPUT.bas OUTPUT.bbc")
    source_path, output_path = map(Path, sys.argv[1:])
    output_path.write_bytes(encode_program(source_path.read_text(encoding="utf-8")))
    print(f"wrote {output_path}")


if __name__ == "__main__":
    main()
