# Bounded `*Obey` parameters and `Obey$Dir` audit

Audit date: 2026-09-30.

This audit covers the hosted positional-parameter and script-directory layer
added to the existing bounded `*Obey <guest-path>` subset. It does not claim
full RISC OS command-script or CLI compatibility. BASIC64 parses the command,
substitutes each source line, and dispatches it through the active command
registry; Rust holds bounded guest-source frames, the invocation's argument
tail and resolved guest directory, and provides the virtual system-variable
view.

## Native reference and hosted contract

The PRM describes `*Obey [[-v][-c] [filename [parameters]]]`: parameters are
space-separated, `%0` names the first parameter, `%*n` names the raw remainder
from parameter `n`, and substitution occurs before a line is passed to the
CLI. `%%0` becomes a literal `%0` with no further substitution during that
pass. `Obey$Dir` is set to the parent part of the pathname used to invoke the
script; that can be only a partial name and can become stale if the current
directory or filing system changes. See [PRM Volume 4, Chapter 85: Command
scripts](https://www.riscos.com/support/developers/prm/commandscripts.html).
The generic argument-substitution routine likewise defines `%0` as the first
of a space-separated argument list; see [PRM Volume 1, Chapter 18:
Conversions](https://www.riscos.com/support/developers/prm/conversions.html).
CLI quoting is a separate later interpretation stage; see [PRM Volume 1,
Chapter 24: The CLI](https://www.riscos.com/support/developers/prm/cli.html).
The system-variable reference describes `Obey$Dir` as available to commands
within the running Obey file; see [PRM Volume 4, Chapter 91: System
variables](https://www.riscos.com/support/developers/prm/systemvars.html).

| Behavior | Hosted contract | Compatibility boundary |
|---|---|---|
| Argument tokenization | The invocation tail is retained per script frame. Positional tokens split on ASCII spaces outside double quotes; quote characters remain in substituted text so the receiving CLI command applies its own syntax. Empty quoted tokens remain present. Unmatched argument quotes reject the invocation before its first line. | This is a bounded tokenizer, not `OS_ReadArgs` or a general shell parser. Tabs do not delimit parameter tokens. The path is parsed separately and may be quoted to contain spaces. |
| `%0`–`%9` | Single-digit indices address parameters from zero; a missing parameter expands to empty. `%10` means `%1` followed by literal `0`. Arguments inserted by substitution are not rescanned as templates. | No numeric multi-digit parameter indices or option parsing. CLI aliases are a separate preprocessing layer applied when each expanded line is dispatched; see the [CLI alias audit](ricochet-cli-aliases-audit.md). General command expansion remains unsupported. |
| `%*n` | Inserts the original raw suffix beginning at the first byte of parameter `n`, including subsequent separators and trailing bytes; the separator preceding that parameter is excluded. A missing parameter gives an empty suffix. | This is not a reconstructed/normalized argument list. Its resulting bytes are still parsed by the receiving command, so quotes and escapes can affect that command. |
| Percent forms | `%%` emits a literal percent protected from another substitution during that pass. `%*` without a digit and `%*` followed by a non-digit are errors. Other unrecognized `%x` pairs are retained literally. | This does not implement later CLI alias substitution or recursive rescanning. |
| Bounds and errors | Each expanded command is rejected if it exceeds 255 UTF-8 bytes; it is never silently truncated. A malformed expansion fails with the current guest path and physical line. The current line is not dispatched, later lines stop, and prior completed effects remain. | The containing script is not transactional; completed filesystem/variable effects are not rolled back. |
| `Obey$Dir` visibility | While a frame is active, exact `OS_ReadVarVal` reads and wildcard enumeration expose a virtual String value; type-0 string expansion resolves `<Obey$Dir>` from the top frame. Nested frames expose their own directory. | Hosted value is the checked, resolved guest parent path (for example `HostFS::DemoDisk.$.parent`), not the PRM's invoked-path parent fragment. It is guest syntax only; no host path is exposed. This is an intentional stability/containment deviation. |
| `Obey$Dir` scope and writes | The overlay is keyed by the original Task and active source frame. It shadows any stored variable only while active, including wildcard reads; selectors matching `Obey$Dir` cannot update or delete the backing entry during an active frame. Closing, error unwind, and `QUIT` reveal the previous stored type/value or absence. | Obey grants no new `SystemVariableWrite` authority. Ordinary callers retain their existing denial; no global process/environment variable is created. |

The bounded source format remains the one documented in
[`ricochet-obey-audit.md`](ricochet-obey-audit.md): UTF-8 with LF/CRLF/CR,
preflight rejection of embedded NUL and unsupported controls, CLI comments,
bounded nested reads, source/line provenance, fail-stop behavior, and explicit
work ceilings. `-v` and `-c`, Exec, Run$Path, automatic `!Boot`,
general GSTrans remain deferred. The separate BASIC command-surface cleanup
is recorded in
[`ricochet-command-script-milestone-audit.md`](ricochet-command-script-milestone-audit.md).
This is not a claim that arbitrary RISC OS Obey scripts run unchanged.

## Review evidence

The implementation keeps policy in `modules/RicochetCommands.bas64`:
`ParseObeyPath`, `ValidateObeyArguments`, and `ExpandObeyLine` parse the tail,
perform one-pass substitution, enforce the expanded-command limit, and fail
through the ordinary CLI error path. `src/swi.rs` stores the invocation tail
and resolved guest parent on each caller-task frame, preflights and bounds
source access, and unwinds frames/buffers on errors. `src/system_variables.rs`
provides the task/frame-supplied overlay for exact and wildcard reads and
type-0 expansion; the OS_SetVarVal service enforces the existing caller right
and rejects selectors matching the active reserved variable. No host
filesystem path or provider-granted caller authority crosses this boundary.

`tests/ricochet_obey_parameters.rs` covers positional indices including `%9`,
missing indices, `%*0`/`%*1`/`%*9`, raw suffix spacing, `%10`, `%%`, no-rescan,
quoted and empty parameters, malformed quoting/`%*`, oversized UTF-8
expansion, nested independent argument tails, and task-local `Obey$Dir`
visibility/restoration after success, nested calls, failure, and `QUIT`. It
also checks shadowing/restoration of stored type-0/type-4 values and absence,
wildcard visibility, denied ordinary writes, preservation of the stored value
after a forged assignment, and stdio input remaining queued. The existing
`tests/ricochet_obey.rs` and `tests/ricochet_obey_input_regressions.rs` cover
the earlier source, cleanup, authority, input-preflight, and line-provenance
contracts.

Independent focused verification on 2026-09-30 used a fresh
`/tmp/ricochet-obey-parameters-audit.51rHlC` configuration and volume. The
command `cargo test --no-default-features --test ricochet_obey_parameters
--test ricochet_obey --test ricochet_obey_input_regressions` passed all 14
tests (4 parameter, 6 base Obey, 4 input regressions). Independent full
verification used a separate fresh `/tmp/ricochet-obey-parameters-full.4oveRV`
configuration and volume. `cargo test --no-default-features` exited
successfully: 218 unit tests passed, 4 GPU-dependent tests were ignored, and
all integration tests and doc tests passed. `git diff --check` passed for the
reviewed implementation/docs/test paths. `cargo fmt --all -- --check` also
reported unrelated workspace formatting diffs; a scoped
`rustfmt --edition 2024 --check src/swi.rs src/system_variables.rs
tests/ricochet_obey_parameters.rs` passed.

These results validate this bounded command-script addition only. They do
not complete the command-environment phase or establish full PRM script
semantics.
