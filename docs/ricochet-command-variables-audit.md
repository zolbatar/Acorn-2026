# Ricochet bounded command-variable and string-expansion audit

Audit snapshot: 2026-09-30. Scope: the runtime-local string-variable store,
BASIC64 `*SET`/`*UNSET`/`*SHOW`, hosted `OS_ReadVarVal`/`OS_SetVarVal`, and
bounded immediate expansion on type-0 writes. This is not complete RISC OS
system-variable or GSTrans compatibility.

## Primary contract

The official [RISC OS PRM Program Environment chapter](https://www.riscos.com/support/developers/prm/progenv.html)
documents `OS_ReadVarVal` (`&23`), `OS_SetVarVal` (`&24`), and the related star
commands. Relevant contract points:

| Surface | PRM behavior relevant to this audit |
|---|---|
| Variable identity | Lookup is case-insensitive; creation spelling is retained. Names may contain any non-space, non-control characters. `*` and `#` patterns are supported for reads/enumeration and for updating/deleting existing variables; they do not create wildcard-named variables. |
| `OS_ReadVarVal &23` | R0 name pointer; R1 value buffer; R2 capacity, or bit 31 set for existence/length probing; R3 wildcard enumeration context; R4=3 requests conversion of number/macro to string. On ordinary reads R0/R1 are preserved, R2 is returned byte length, R3 next context and R4 variable type. Returned values are length-delimited, not NUL-terminated. With bit-31 probing, absent returns R2=0; present remains negative and, unless R4=3, R2 is `NOT(length)`. The probe may raise an error, so callers needing non-error enumeration use X form. |
| `OS_SetVarVal &24` | R0 name; R1 value; R2 length or negative for delete; R3 wildcard context; R4 type. R0-R2 are preserved, R3 becomes the next context and R4 reports an evaluated type. Creation names may not contain wildcard/control characters. Delete/update may target existing wildcard matches. |
| Types and strings | String (0) is GSTrans'd immediately; Number (1) is a signed 4-byte integer; Macro (2) GSTrans'es when read; Expanded (3) evaluates an expression; LiteralString (4) is raw; Code (16) installs machine-code callbacks. Non-literal string writes terminate by LF/CR/NUL. `*SET` performs immediate GSTrans; `*SetMacro`, `*SetEval`, and code variables have distinct behavior. |
| Star commands | `*SET name value` assigns a string; `*UNSET variable_spec` deletes a name/pattern; `*SHOW [variable_spec]` reports name, type, and value, and no-argument `*SHOW` enumerates all variables. Missing is distinct from an existing empty string. |

The official [RISC OS PRM Conversions chapter](https://www.riscos.com/support/developers/prm/conversions.html)
documents `<name>` substitution, numeric `<number>` character conversion,
and `|` control/escape syntax. Its example substitutes a variable's returned
value for the reference. This batch implements exact named substitutions,
outer double quotes (for preserving leading spaces), doubled quotes, and the
printable escapes `|<`, `|>`, `||`, and `|"`. Numeric conversion, generated
controls, and other escapes are deliberately excluded.

These are the baseline semantics, not an assumption that every behavior belongs
in this batch. Any narrower register/type/probe/wildcard contract must be stated
as a deliberate hosted deviation and must reject unsupported behavior without
partial output or mutation.

## Hosted contract observed in source

The project direction keeps BASIC64 responsible for public command parsing,
diagnostics, and policy; Rust may own bounded storage and checked host
mechanisms. Guest pointers must remain caller-scoped logical addresses. This
batch must use one runtime-local store (shared by tasks using that runtime,
isolated from another runtime), must not import or mutate the host process
environment, and must not persist the values. `ConfigurationWrite` is not
variable-write authority: mutation requires its own Task right and primitive
capability, granted explicitly only to the trusted MOS bootstrap. An ordinary
task's direct SWI and nested `OS_CLI` attempts must be denied before state
changes, with the original requestor preserved through BASIC64/provider calls.

The inspected implementation publishes PRM-numbered `OS_ReadVarVal &23` and
`OS_SetVarVal &24` from `System.bas64`; those definitions forward the original
register set and caller `Task` to one `SwiDispatcher`-owned `SystemVariableStore`.
The store uses ASCII case-folded keys while retaining first-creation spelling,
an ordered map for deterministic enumeration, and bounded validation before
insert/update/delete. The source bounds names to 1–32 visible ASCII bytes,
values to 256 valid UTF-8 bytes, the store to 128 entries and 32 KiB of
name-plus-value storage. Mutations compute prospective totals before changing
the map. `*` is zero-or-more and `#` is one-byte matching; update must select
exactly one existing match and wildcard delete removes its complete match set
atomically.

The ABI keeps checked logical addresses for R0/R1 data and uses R3 as an opaque
caller-owned logical dynamic-area token containing the matched NUL-terminated
name. The Task keeps the selector and sorted continuation key alongside that
token, rejects foreign, mismatched, or modified tokens, and allows interleaved
enumerations. Values returned through R1 remain length-delimited (no appended
NUL). Exact reads check capacity and preflight the complete output range before
writing; existence/length probes leave R1 untouched. Type-0 writes require the
LF/CR/NUL terminator exactly at R1+R2. Delete is any negative R2. The handlers
preserve documented input registers on success and route normal/X failures
through the task-scoped runtime error contract.

`RicochetCommands.bas64` owns the three registered commands. `*SET` writes type
0 through `OS_SetVarVal`, preserving the value tail after separator whitespace;
`*UNSET` delegates exact or wildcard deletion; `*SHOW` uses X-form wildcard
`OS_ReadVarVal` to enumerate the shared store, labels type 0 vs type 4, and
renders validated strings through the normal text channel. Exact-name `SHOW`
filters case-insensitively; no-argument `SHOW` enumerates all. The live command
registry puts `STATUS` before `SET`/`SHOW`, so execution of `*S.` retains the
existing Status priority while Help can list all three prefix matches.

Authority is a separate private `SystemVariableWrite` right, checked in the
Rust `Host.SystemVariables.Write` mechanism before reading guest pointers. Only
the host-created trusted MOS-session Task receives it. `Task::new`, source-only,
module-only, and configuration-only tasks do not; numeric task IDs cannot
select a profile. `RicochetCommands` reaches the public System SWI, not the
private write primitive, so nested dispatch carries the same original Task and
cannot borrow authority from the System provider. Reads are intentionally public
within that runtime. Each runtime/dispatcher owns its own empty store; nothing
imports the host environment or persists values.

## Deliberate hosted deviations

- These are System Profile compatible names/registers, not full historical
  system-variable services. Only String (0) and LiteralString (4) values are
  stored. Number (1), Macro (2), Expanded (3), and Code (16) writes are rejected
  before mutation. `R4=3` on read returns the same stored bytes; no numeric or
  read-time conversions exist.
- Selector strings are NUL-terminated only. PRM OS_SetVarVal permits a name
  terminator at any byte value `<= 32`; that broader terminator convention is
  not implemented. The hosted handlers also use Ricochet structured error
  categories/codes and task-scoped X error blocks rather than reproducing the
  historical PRM error vocabulary and numbers.
- Type 0 immediately expands exact `<name>` references, outer double quotes,
  doubled quotes, and printable escapes `|<`, `|>`, `||`, `|"`. Referenced
  bytes are appended once and never rescanned, following the PRM's direct
  substitution description. Numeric operands, wildcard references, unsupported
  escapes, malformed syntax, and missing variables fail before mutation. Type
  4 remains raw UTF-8. Input and output are limited to 256 bytes; both types
  reject controls so `*SHOW` cannot inject terminal/VDU controls. This is a
  deliberate subset, not an `OS_GSTrans` implementation or public contract.
- Names require visible ASCII and a 32-byte maximum. PRM allows a broader
  non-space/non-control alphabet and does not impose this hosted bound.
- R3 wildcard read contexts are task-local logical allocations rather than the
  historical module's internal context representation. The hosted write
  interface requires R3=0 and does not expose incremental wildcard-write
  iteration: a single-match wildcard update or an atomic all-match delete is
  the supported behavior.
- `*SET <name> [value]` intentionally treats an omitted value as an existing
  empty String variable. The PRM command syntax shows a required value operand.
  Empty and absent are consequently distinct in this hosted store.
- The existence probe preserves R0 (PRM permits it to be corrupted); it
  returns R2=0 when absent and a negative value when present. For raw requests
  it returns `NOT(length)`; R4=3 remains negative with the requested length.
- This store is runtime-session communication state, not host environment
  state or persistent CMOS/configuration state. No variables are preloaded.

## Verification and remaining scope

The focused public variable integration passes 4/4. The added expansion suite
passes 2/2. The full `cargo test --no-default-features -- --test-threads=1`
suite passed with isolated `RICOCHET_CONFIG_PATH` and
`RICOCHET_DEMO_VOLUME` values: the library reported 218 passed, 0 failed, and
4 ignored (GPU-required); every integration and doc-test target passed,
including variable, Help, authorization, command compatibility,
configuration, recovery, and WP5.1 targets. One initial whole-suite run had a
transient existing desktop/Filer paging assertion failure; that exact test
passed alone and the complete rerun passed. The historical baseline's 216
library passes increased to 218 from the new expansion unit tests.

`tests/ricochet_variable_expansion.rs` pins the high-bit R4=3 probe pattern,
rejects nonzero OS_SetVarVal R3 and non-NUL selector termination, and checks
literal writes, composition, non-rescanning, quote/escape handling, unsupported
syntax, missing variables, and atomic failures. This is self-reviewed
black-box coverage, not an independent peer audit.

The narrower parser contract and source interpretation are recorded in the
[`string-expansion audit`](ricochet-variable-expansion-audit.md).

`tests/ricochet_command_variables.rs` exercises direct SWI register and buffer
behavior, CLI/SWI interoperability, wildcard enumeration/update/delete,
interleaved contexts, authority denial, runtime isolation, no environment
import/persistence, and control-safe output.

No numeric/macro/code conversion, public `OS_GSTrans` contract, native Type-2
macro variables, Obey/Exec scripts, general command substitution, paging, or
native command order is included. The bounded BASIC command-family cleanup is
recorded separately in the [command/script milestone audit](ricochet-command-script-milestone-audit.md);
native Type-2 alias macros and full PRM command processing remain unsupported.
Passing tests do not establish those omitted facilities; this audit does not
claim complete PRM compatibility or completion of the wider command-environment
phase.
