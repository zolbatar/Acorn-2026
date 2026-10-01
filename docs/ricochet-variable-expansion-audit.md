# Bounded system-variable string expansion

Audit snapshot: 2026-09-30. This records a useful immediate-expansion subset
for type-0 system-variable writes. It is not a full GSTrans implementation and
does not publish an `OS_GSTrans` SWI or claim full PRM compatibility.

## Source-backed behavior

The [RISC OS PRM Program Environment chapter](https://www.riscos.com/support/developers/prm/progenv.html)
defines type 0 as a string translated by GSTrans when written, while type 4 is
literal. The [PRM Conversions chapter](https://www.riscos.com/support/developers/prm/conversions.html)
documents `<name>` system-variable substitution and shows a referenced
variable's value replacing that token. It also documents `<number>` character
conversion, `|` control/escape forms, and quote handling: enclosing double
quotes permit leading spaces and are stripped, doubled quotes encode a quote,
and `|<`, `|>`, `||`, and `|"` encode printable delimiters.

The explicit documented `<name>` example supports one replacement pass. This
host appends the bytes returned for that variable once; it never interprets
those inserted bytes as fresh syntax. That prevents recursive or attacker
controlled rescanning while preserving useful variable composition.

## Hosted subset

Type-0 writes accept:

- Exact `<name>` references to an existing variable, using the store's
  case-insensitive lookup. Names are visible ASCII and at most 32 bytes.
- Double quotes only when they enclose the complete input string. Those outer
  quotes are removed. Doubled quotes inside an outer-quoted string represent
  one quote; `|"` also represents one quote.
- Printable literal escapes `|<`, `|>`, and `||`.
- Ordinary valid UTF-8 characters, excluding controls.

Type-4 writes store their bytes as literal UTF-8 and do not parse angle
brackets, quotes, or pipes. Both input and expanded output have a 256-byte
maximum. The expansion scan has a finite work ceiling. The existing store
limits, task authority, logical-memory checks, and no-host-environment/no-
persistence rules remain in force. `*SHOW` receives only validated printable
values and cannot emit terminal controls.

Malformed angle syntax, empty/invalid names, numeric `<number>`, wildcard
references, unterminated or partial quote forms, undoubled interior quotes,
unknown `|` forms, controls, oversized output, and missing variables fail
before the map is updated. Missing references return the hosted
`SystemVariableNotFound` error. Unsupported syntax returns
`SystemVariableExpansionError`. These error categories are Ricochet behavior,
not historical PRM error numbers. Wildcard SET destination selection remains
the separate existing store rule: exactly one existing match is required.

Rust performs this bounded translation at the shared store boundary because
both public `OS_SetVarVal` and BASIC64 `*SET` must reach identical immediate
type-0 behavior. The module remains the public SWI and CLI owner; no independent
GSTrans entry point, command-tail expansion, alias evaluation, or script
execution is implied.

## Exclusions

This slice excludes numeric conversion, `|`-generated control bytes, command
parameters such as `|%0`, host/environment values, dynamic OS_GSReadable
providers, macro expansion on read, expression evaluation, code variables,
arbitrary OS_CLI expansion, aliases, paths/redirection, Obey/Exec, and recursive
expansion. Literal type 4 is not subject to the type-0 parser.

## Regression evidence

`tests/ricochet_variable_expansion.rs` exercises the public SWI dispatch and
guest logical buffers for type-0 composition, non-rescanning, literal type 4,
quoted leading spaces, printable escapes, malformed and missing references,
atomic failure, the high-bit R4=3 probe result, nonzero write R3 rejection,
and NUL-only selector termination (2/2). The focused pre-existing variable
suite passes 4/4. The full `cargo test --no-default-features --
--test-threads=1` rerun passed with 218 library tests, 4 GPU-required ignored,
and all integration/doc-test targets passing. `system_variables` unit tests
cover store limits and replacement atomicity. One first full run had a
transient desktop/Filer paging assertion; its standalone rerun and the complete
suite rerun passed. This audit was prepared by the implementation agent and is
not independent peer review. The adjacent
[`command-variable audit`](ricochet-command-variables-audit.md) records the
broader store, authority, and runtime-session contract.
