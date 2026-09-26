#!/usr/bin/env python3
"""Convert learnagon's text BASIC examples to the project's ARM token stream.

The external `Tokenize` utility writes ARM BASIC V records using the shared-CR
layout. This script retains its token bytes and line references while framing
the records with the separate line terminators used by the project's generated
ARM-profile fixtures. It does not validate BASIC syntax or emulate Agon APIs.
"""

from __future__ import annotations

import argparse
import os
import shutil
import subprocess
import tempfile
from pathlib import Path


CR = 0x0D
END = 0xFF


def decode_shared_records(data: bytes) -> list[tuple[int, bytes]]:
    if len(data) < 2 or data[-2:] != bytes((CR, END)):
        raise ValueError("Tokenize output has no BASIC end marker")

    lines: list[tuple[int, bytes]] = []
    cursor = 0
    previous: int | None = None
    while cursor < len(data):
        if data[cursor] != CR:
            raise ValueError(f"expected a line marker at byte {cursor}")
        if data[cursor + 1] == END:
            if cursor + 2 != len(data):
                raise ValueError("bytes follow the BASIC end marker")
            return lines
        if cursor + 4 > len(data):
            raise ValueError(f"truncated line header at byte {cursor}")

        number = int.from_bytes(data[cursor + 1 : cursor + 3], "big")
        length = data[cursor + 3]
        end = cursor + length
        if length < 4 or end >= len(data) or data[end] != CR:
            raise ValueError(f"invalid record length for BASIC line {number}")
        if previous is not None and number <= previous:
            raise ValueError("BASIC line numbers are not strictly increasing")

        lines.append((number, data[cursor + 4 : end]))
        previous = number
        cursor = end

    raise ValueError("Tokenize output has no BASIC end marker")


def encode_separate_records(lines: list[tuple[int, bytes]]) -> bytes:
    output = bytearray()
    for number, body in lines:
        if not 0 <= number <= 0xFEFF:
            raise ValueError(f"BASIC line number {number} is out of range")
        record_length = len(body) + 5
        if record_length > 0xFF:
            raise ValueError(f"BASIC line {number} is too long for the saved format")
        output.extend((CR, number >> 8, number & 0xFF, record_length))
        output.extend(body)
        output.append(CR)
    output.extend((CR, END))
    return bytes(output)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--source-root",
        type=Path,
        default=Path("examples/bbc-basic-by-example"),
        help="directory containing the imported .BAS files",
    )
    parser.add_argument(
        "--tokenizer",
        default=os.environ.get("BBC_BASIC_TOKENIZER", "tokenize"),
        help="path to Steve Fryatt's ARM BASIC V Tokenize executable",
    )
    args = parser.parse_args()

    source_root = args.source_root.resolve()
    tokenizer = shutil.which(args.tokenizer) or args.tokenizer
    sources = sorted(
        path for path in source_root.rglob("*") if path.is_file() and path.suffix.lower() == ".bas"
    )
    if not sources:
        parser.error(f"no .BAS sources found under {source_root}")

    for source in sources:
        target = source.with_suffix(".bbc")
        with tempfile.NamedTemporaryFile(prefix="acorn-tokenize-", suffix=".bbc") as raw_file:
            result = subprocess.run(
                # The corpus metadata comment is deliberately kept at BASIC line 0.
                [tokenizer, str(source), "-start", "0", "-out", raw_file.name],
                capture_output=True,
                text=True,
            )
            if result.returncode != 0:
                message = (result.stderr or result.stdout).strip()
                raise RuntimeError(f"Tokenize failed for {source}: {message}")
            raw_file.seek(0)
            lines = decode_shared_records(raw_file.read())

        target.write_bytes(encode_separate_records(lines))
        print(f"{source.relative_to(source_root)}: {len(lines)} lines -> {target.name}")

    return 0


if __name__ == "__main__":
    raise SystemExit(main())
