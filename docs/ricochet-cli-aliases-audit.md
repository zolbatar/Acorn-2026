# Bounded CLI aliases and bypass audit

This audit records the hosted alias layer added to the existing BASIC64-owned
`OS_CLI` dispatcher. It is a deliberately bounded subset, not full RISC OS
command preprocessing, GSTrans, or macro-variable compatibility. Alias policy
stays in `modules/RicochetCommands.bas64`; Rust supplies the existing checked
system-variable SWIs/store and command registry mechanisms.

## Primary compatibility references

The [RISC OS PRM Volume 1, Chapter 24: The CLI](https://www.riscos.com/support/developers/prm/cli.html)
documents command preprocessing, leading-star/space handling, `|` comments,
`%` alias bypass, aliases taking precedence over command lookup, final-dot
abbreviations, positional alias substitution, and recursive `OS_CLI` calls.
The [PRM Volume 1, Chapter 18: Conversions](https://www.riscos.com/support/developers/prm/conversions.html)
documents `OS_SubstituteArgs`: space-separated parameters with quotes retained
for later command interpretation, `%0`–`%9`, `%*n`, `%%`, missing parameters,
unused-parameter appending, and a bounded output buffer. The
[PRM Volume 1, Chapter 15: Program Environment](https://www.riscos.com/support/developers/prm/progenv.html)
describes system-variable types, including dynamic Macro type 2, and wildcard
inspection such as `*Show Alias$*`.

## Hosted contract

| Area | Implemented behavior | Explicit boundary |
|---|---|---|
| Definition and storage | `Alias$<command>` is read live from the existing shared system-variable store. Alias definitions are ordinary authorized `*SET`/`OS_SetVarVal` values of supported String type 0 or LiteralString type 4; updates and `*UNSET` take effect immediately. `*SHOW Alias$*` enumerates actual definitions. | Native Alias$ definitions are Macro type 2 and are dynamically interpreted. Ricochet has no Macro type, so aliases here are static current strings. Type 0 performs the store's immediate bounded expansion at assignment; use type 4 when a raw target is required. There is no separate alias table or synthetic command-registry entry. |
| Command names | Lookup is case-insensitive and uses a bounded command-name suffix (at most 26 visible ASCII bytes; letters/digits plus ``! ' ( ) + - . ; = ? @ [ ] _ ` { } ~``). An exact alias is checked first. Final-dot lookup enumerates using the fixed, valid `Alias$*` selector, then filters complete candidate suffixes through the same name validator and checks for one unique prefix; ambiguous prefixes error. This avoids creating a 33-byte wildcard selector for a 26-byte command suffix while preserving the existing 32-byte system-variable name limit. Alias resolution precedes the live command registry, so an alias can deliberately shadow a registered name; without a valid matching alias, existing registry order and abbreviations continue unchanged. | This does not model PRM's environment-dependent assembly of module/Filing-System command aliases. Invalid alias-name suffixes remain visible as variables but are ignored by command lookup. The extracted PRM punctuation rendering after `_` is ambiguous, so this audit does not claim that hosted backtick support is an exact native spelling match. |
| Leading `%` | After the shared leading `*`/space/tab normalization and `|` comment classification, one leading `%` skips alias lookup and dispatches the remaining command text normally. | `%` is not general variable substitution or a recursive bypass mode. It does not enable host executable/file lookup. |
| Parameters | Arguments split on ASCII spaces outside double quotes; quote characters remain for the receiving command's parser, empty quoted arguments are present, and unmatched quotes fail. `%0`–`%9` substitute one token (missing is empty); `%*n` inserts the raw suffix beginning at token *n*, retaining separators after that token and trailing spaces but not the separator before it. `%%` emits a literal percent for that pass; `%10` is `%1` then `0`; unrecognized pairs such as `%q` remain literal. Unused arguments after the highest single-digit slot referenced are appended unless `%*n` supplied the tail. | This is the `OS_SubstituteArgs`-inspired subset, not `OS_ReadArgs`, shell quoting, option parsing, or general command expansion. Tabs separate the CLI verb from its tail but do not delimit alias parameter tokens. |
| Dispatch and bounds | The expanded command is reparsed by the same BASIC64 CLI normalizer, then resolved against the same active registry. Alias-to-alias calls are allowed, with cycle detection, an eight-expansion depth limit, at most 255 UTF-8 bytes per expanded command, and at most 2,048 UTF-8 bytes of aggregate expansion work per CLI dispatch. Empty targets, malformed `%*` forms, unmatched quotes, ambiguity, cycles, excess depth, and excess length/work fail explicitly rather than truncating. | One alias target is one command. There is no macro body, multi-command alias, pipe/redirection, general GSTrans, `Exec`, Run$Path, automatic file execution, or arbitrary host-path/environment fallback. These omissions must not be inferred as supported from the recursive alias chain. |
| Help and observability | `*HELP ALIASES` describes the subset; wildcard `*SHOW Alias$*` reads current store state. Help metadata remains the normal command registry's static topic, while alias values stay in the variable store. | Help does not manufacture a registry row for each alias. It does not claim all PRM Alias$ features. |
| Rights, provenance, and cleanup | Alias substitution does not change the original caller Task. The final registry invocation receives that same Task, so protected `SystemVariableWrite`, `ModuleManagement`, and other checks remain in force. An alias invoked within `*Obey`/`*RMEnsure` retains the active guest source path/line on errors. Command scratch and alias lookup context are released on success/error; subsequent commands and queued stdio input remain usable. | Alias definitions are session-local with the variable store, not persistent or inherited from the host. No rights are borrowed from the task that created an alias. |

## Implementation and evidence

The shared CLI preprocessing and alias policy are in
[`modules/RicochetCommands.bas64`](../modules/RicochetCommands.bas64)
(`DispatchCliText`, `ResolveCommandAlias`, `ValidateAliasKey`, `ExpandAliasTarget`). Alias values
are read through the existing `OS_ReadVarVal` interface; the existing
caller-checked variable mechanism and private write right remain in
[`src/swi.rs`](../src/swi.rs) and [`src/system_variables.rs`](../src/system_variables.rs).
The active registry and handler invocation are still the source of actual
command behavior. The focused integration evidence is
[`tests/ricochet_cli_aliases.rs`](../tests/ricochet_cli_aliases.rs) and
[`tests/ricochet_alias_boundaries.rs`](../tests/ricochet_alias_boundaries.rs), alongside
the existing command Help, Obey, and authorization suites.

Independent validation on 2026-09-30 used unique temporary configuration and
volume paths. The focused command
`cargo test --no-default-features --test ricochet_alias_boundaries --test ricochet_cli_aliases --test ricochet_obey --test ricochet_obey_parameters --test ricochet_authorization --test ricochet_command_variables --test ricochet_command_help`
passed all 22 tests. This includes live create/update/delete and SHOW, Help
not inventing alias registry entries, exact/abbreviated shadowing and `%`
bypass, retained `*S.`/`*D.`/`*F.` command priority absent aliases,
`%0`–`%9`/`%*n`/`%%`/`%10`/unknown escape and unused-argument cases, quoted
and empty arguments, malformed placeholders, cycle/depth/byte bounds,
maximum 26-character names for exact, dotted-full, and unique 25-character
prefix lookup, maximum-length prefix ambiguity and recovery, no-match registry
fallback, invalid-suffix filtering, caller-right preservation, nested Obey
provenance/cleanup, and stdio input.

The independent full command `cargo test --no-default-features` exited 0:
218 unit tests passed and four GPU-only tests were ignored; all integration
targets (including both alias targets) and doc tests passed. `rustfmt --check
--edition 2024 src/swi.rs src/system_variables.rs
tests/ricochet_cli_aliases.rs tests/ricochet_alias_boundaries.rs`,
`git diff --check`, and a trailing-whitespace scan of this new audit document
passed. Repository-wide `cargo fmt --all --
--check` is not clean because it reports pre-existing formatting differences
in unrelated files (including `src/basic_compat/jit.rs`, `src/configure.rs`,
`src/renderer.rs`, `src/main.rs`, and unrelated tests); those files were not
modified for this audit. The acceptance claim is limited to the tested
behavior above; it is not a statement that the RISC OS CLI phase, all PRM
command semantics, or the broader Ricochet implementation is complete.

## Remaining gaps

- Native Alias$ Macro type 2, arbitrary GSTrans evaluation, command-body
  chaining, and all environment-dependent filing-system/module command
  resolution are outside this slice.
- `/Run`, `-` filing-system context overrides, command output/input
  redirection, pipelines, `Exec`, Run$Path/File$Path, and automatic `!Boot`
  execution are not implemented by this alias feature.
- The variable store and command registry remain bounded hosted services;
  this audit does not claim full RISC OS CLI error blocks, interactive
  abbreviation order across native modules, or shell compatibility.
