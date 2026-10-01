# Type-0 system-variable quote correction

Audit date: 2026-09-30. This records the correction to the bounded type-0
string expansion subset in [the variable-expansion audit](ricochet-variable-expansion-audit.md).

The parser previously guessed whether terminal quote bytes closed a quoted
value by counting quote and pipe parity. That rejected valid `""` and
`"a|""` values. The parser now scans the existing grammar sequentially:
`|"` is data wherever encountered, a raw quote closes a whole-input quoted
value, and doubled quotes inside it produce one literal quote. The byte scan
preserves UTF-8 boundaries. A dangling escape, unescaped interior quote,
missing delimiter, or escaped quote without a later closing delimiter is
rejected before the store changes.

This is deliberately narrower than full PRM GSTrans: only whole-input outer
quotes (including empty `""`), doubled quotes within them, and printable
`|<`, `|>`, `||`, and `|"` escapes are implemented. Numeric/control
conversions, command parameters, recursive substitution, and other GSTrans
syntax remain excluded. Type 4 is still raw, substitutions are still
appended once without rescanning, and the existing byte/work limits,
validation, caller authority, and atomic write boundary remain unchanged.

Regression coverage is in `tests/ricochet_variable_quote_regressions.rs` and
`tests/ricochet_quote_state_machine.rs`: direct SWI cases cover escapes at
each position, empty and non-empty quoted forms, escaped and doubled quotes,
UTF-8, malformed-input atomicity, type 4, one-pass substitutions, the
256-byte boundary, and ordinary-task write denial; a public `--stdio` case
covers the reported command sequence. These tests demonstrate those listed
paths only; they do not establish full PRM GSTrans compatibility.
