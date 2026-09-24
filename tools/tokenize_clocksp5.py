#!/usr/bin/env python3
"""Encode the ClockSP5 text fixture as an ARM BBC BASIC V saved program.

This deliberately covers the tokens and source conventions used by this
fixture. It is not a general BBC BASIC tokenizer or syntax checker.
"""

from __future__ import annotations

import re
import sys
from pathlib import Path


TOKENS = {
    "OTHERWISE": 0x7F,
    "AND": 0x80,
    "DIV": 0x81,
    "EOR": 0x82,
    "MOD": 0x83,
    "OR": 0x84,
    "ERROR": 0x85,
    "LINE": 0x86,
    "OFF": 0x87,
    "STEP": 0x88,
    "SPC": 0x89,
    "ELSE": 0x8B,
    "THEN": 0x8C,
    "ABS": 0x94,
    "ACS": 0x95,
    "ADVAL": 0x96,
    "ASC": 0x97,
    "ASN": 0x98,
    "ATN": 0x99,
    "COS": 0x9B,
    "EVAL": 0xA0,
    "EXP": 0xA1,
    "FALSE": 0xA3,
    "FN": 0xA4,
    "INKEY": 0xA6,
    "INT": 0xA8,
    "LEN": 0xA9,
    "LN": 0xAA,
    "LOG": 0xAB,
    "NOT": 0xAC,
    "RND": 0xB3,
    "SQR": 0xB6,
    "TAN": 0xB7,
    "TO": 0xB8,
    "TRUE": 0xB9,
    "CHR$": 0xBD,
    "GET$": 0xBE,
    "INKEY$": 0xBF,
    "LEFT$(": 0xC0,
    "MID$(": 0xC1,
    "RIGHT$(": 0xC2,
    "STR$": 0xC3,
    "STRING$(": 0xC4,
    "ENDCASE": 0xCB,
    "ENDIF": 0xCD,
    "ENDWHILE": 0xCE,
    "DIM": 0xE2,
    "SOUND": 0xD4,
    "CALL": 0xD6,
    "DATA": 0xDC,
    "DEF": 0xDD,
    "END": 0xE0,
    "ENDPROC": 0xE1,
    "FOR": 0xE3,
    "GOSUB": 0xE4,
    "GOTO": 0xE5,
    "IF": 0xE7,
    "INPUT": 0xE8,
    "NEXT": 0xED,
    "PRINT": 0xF1,
    "PROC": 0xF2,
    "READ": 0xF3,
    "REM": 0xF4,
    "REPEAT": 0xF5,
    "RESTORE": 0xF7,
    "RETURN": 0xF8,
    "UNTIL": 0xFD,
}

PSEUDO_VARIABLES = {
    "PTR": (0x8F, 0xCF),
    "PAGE": (0x90, 0xD0),
    "TIME": (0x91, 0xD1),
    "LOMEM": (0x92, 0xD2),
    "HIMEM": (0x93, 0xD3),
}
BRANCH_TOKENS = {"GOTO", "GOSUB", "RESTORE", "THEN", "ELSE"}
TOKEN_NAMES = sorted((*TOKENS, *PSEUDO_VARIABLES), key=len, reverse=True)


def encode_line_reference(line_number: int) -> bytes:
    high = (line_number >> 8) & 0xFF
    low = line_number & 0xFF
    marker = (((low >> 6) << 4) | ((high >> 6) << 2)) ^ 0x54
    return bytes((0x8D, marker, (low & 0x3F) | 0x40, (high & 0x3F) | 0x40))


def tokenise_line(text: str) -> bytes:
    if text.startswith("*"):
        return text.encode("ascii")

    output = bytearray()
    index = 0
    quote = False
    line_reference_list = False

    while index < len(text):
        character = text[index]

        if quote:
            output.append(ord(character))
            if character == '"':
                if index + 1 < len(text) and text[index + 1] == '"':
                    output.append(ord('"'))
                    index += 2
                    continue
                quote = False
            index += 1
            continue

        if character == '"':
            quote = True
            output.append(ord(character))
            index += 1
            continue

        if line_reference_list:
            if character.isspace():
                output.append(ord(character))
                index += 1
                continue
            if character == ",":
                output.append(ord(character))
                index += 1
                continue
            if character.isdigit():
                match = re.match(r"\d+", text[index:])
                assert match is not None
                output.extend(encode_line_reference(int(match.group())))
                index += len(match.group())
                if index >= len(text) or text[index] != ",":
                    line_reference_list = False
                continue
            line_reference_list = False

        matched = None
        upper_tail = text[index:].upper()
        for name in TOKEN_NAMES:
            if upper_tail.startswith(name):
                matched = name
                break

        if matched is None:
            output.append(ord(character))
            index += 1
            continue

        next_index = index + len(matched)
        if matched in PSEUDO_VARIABLES:
            after_name = text[next_index:].lstrip(" \t")
            before_name = text[:index].rstrip(" \t")
            at_statement_start = (
                not before_name
                or before_name.endswith(":")
                or re.search(r"\b(?:THEN|ELSE)\s*$", before_name, re.IGNORECASE)
                is not None
            )
            is_assignment_target = at_statement_start and after_name.startswith("=")
            token = PSEUDO_VARIABLES[matched][1 if is_assignment_target else 0]
        else:
            token = TOKENS[matched]
        output.append(token)
        index = next_index

        if matched == "REM":
            output.extend(text[index:].encode("ascii"))
            break
        if matched == "DATA":
            output.extend(text[index:].encode("ascii"))
            break
        if matched in BRANCH_TOKENS:
            after_spaces = index
            while after_spaces < len(text) and text[after_spaces] in " \t":
                output.append(ord(text[after_spaces]))
                after_spaces += 1
            index = after_spaces
            if index < len(text) and text[index].isdigit():
                line_reference_list = True

    return bytes(output)


def encode_program(source: str) -> tuple[bytes, int]:
    output = bytearray()
    line_count = 0
    for raw_line in source.splitlines():
        match = re.match(r"^\s*(\d+)(.*)$", raw_line)
        if match is None:
            if raw_line.strip():
                raise ValueError(f"expected a numbered BASIC line: {raw_line!r}")
            continue

        line_number = int(match.group(1))
        if not 0 <= line_number <= 0xFEFF:
            raise ValueError(f"line number out of BASIC V range: {line_number}")
        body = tokenise_line(match.group(2))
        record_length = len(body) + 5  # four-byte preamble, body and final CR
        if record_length > 255:
            raise ValueError(f"line {line_number} is too long for BASIC V")

        output.extend((0x0D, line_number >> 8, line_number & 0xFF, record_length))
        output.extend(body)
        output.append(0x0D)
        line_count += 1

    output.extend((0x0D, 0xFF))
    return bytes(output), line_count


def main() -> None:
    if len(sys.argv) != 3:
        raise SystemExit("usage: tokenize_clocksp5.py INPUT.bas OUTPUT.bbc")
    source_path, output_path = map(Path, sys.argv[1:])
    encoded, lines = encode_program(source_path.read_text(encoding="utf-8"))
    output_path.write_bytes(encoded)
    print(f"wrote {len(encoded)} bytes across {lines} lines to {output_path}")


if __name__ == "__main__":
    main()
