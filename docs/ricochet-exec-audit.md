# Bounded `*Exec` input-stream audit

This audit records the hosted `*Exec` input-source subset. It is distinct from
`*Obey`: Exec supplies characters to the active input stream, while Obey reads
and dispatches command lines itself. This is not a claim of complete RISC OS
command-line or input-stream compatibility.

## Primary compatibility references

The [RISC OS PRM Volume 2, Chapter 27: FileSwitch](https://www.riscos.com/support/developers/prm/fileswitch.html)
defines `*Exec [filename]`: the file becomes the current input source ahead of
keyboard/serial input, and bare `*Exec` closes it. The [PRM Volume 1, Chapter
23: Character Input](https://www.riscos.com/support/developers/prm/charinput.html)
describes the three input sources, identifies `OS_ReadC` as the core stream
reader, says `OS_ReadLine` reads through it, and documents OS_Byte 198 as the
handle-level switch/stop mechanism. Native OS_Byte 198 can replace the current
Exec handle and returns the displaced handle for the caller to close; it does
not imply a nested input-source stack.

The [BBC BASIC Reference Manual: Keywords](https://www.riscos.com/support/developers/bbcbasic/bbcref.html)
describes `GET`/`GET$` and `INPUT` as reading from the input stream; `INKEY` is
instead a timed keyboard query. These distinctions inform the hosted routing
and the explicit polling boundary below.

## Hosted contract

| Area | Implemented behavior | Boundary or deviation |
|---|---|---|
| Command and path | BASIC64 owns `*Exec [guest-path]` in the live command registry. It accepts one guest path token, or a whole quoted path containing spaces. Bare `*Exec` closes the current task's Exec source. The path is resolved/read through the caller Task's existing HostFS context. | No host-path fallback, parameters, switches, `Run$Path` search, filetype dispatch, or automatic `!Boot` execution. This is not `*Obey` syntax or a direct command-dispatch loop. |
| Source selection | A task has at most one active Exec source. A valid `*Exec new-path` replaces/discards the old source only after the new file has been read and fully checked. A path/read/UTF-8/control/size/line validation failure leaves the old source and its current cursor intact. Bare Exec clears it. | No nested source stack. BASIC64 policy exposes pathname replacement/stop only; the native OS_Byte 198 file-handle API and its register contract remain deferred. |
| Input routing | The active source is consumed before the caller's queued MOS or host-console bytes. The same checked Console input primitive serves MOS `OS_ReadC`/`OS_ReadLine`, BASIC `GET`/`INPUT`, and command-prompt line input. This is actual stream consumption, not a loop that directly dispatches source lines. A different Task cannot consume the source. | While a Task has an active Exec source, opportunistic BASIC `INKEY` polling is suppressed so it cannot take queued host bytes ahead of that source. Those queued bytes remain available when the source ends. This is a hosted queue-preservation policy, not a claim that every native keyboard-query interaction is reproduced. |
| EOF and lines | CRLF and LF normalize to one CR; lone CR is kept. A final unterminated byte sequence is delivered; the line reader sees end-of-input once to complete that partial line, then later reads fall through to the untouched queued/host stream. Normal CR-terminated lines fall through after EOF on the next read. | The hosted source is preloaded, not kept as an open guest file handle. No line echo, `-v`/`-c`, or native file-handle inspection is provided by Exec. |
| Validation and bounds | The complete candidate is checked before it replaces the active source: valid UTF-8; printable text, tab, CR, and LF only; at most 65,536 source bytes, 255 bytes per physical line, and 4,096 physical lines. Errors reject rather than truncate and report a guest path/line where applicable, without exposing a host path. | These are hosted safety ceilings, not PRM limits. The input is UTF-8 text, not an arbitrary binary stream. |
| Authority and cleanup | Rust stores the bounded source/cursor and provenance on the original `Task`; BASIC64 owns the command policy. The read uses that Task's existing checked guest filesystem context and does not add file, system-variable, or module rights. Bytes are buffered, so no host file handle remains open. Bare stop, replacement, EOF, QUIT, and Task teardown clear or release the source state; OS_CLI command scratch is cleaned at its public boundary on success/error. | This is runtime-session state, not a global or host environment variable and not shared between unrelated Tasks. |

The native PRM documents a file as a current input source and OS_Byte 198 as
the lower-level handle switch/termination control. Ricochet intentionally
implements only the checked path-based command interface; no OS_Byte 198
semantics are implied. File input is isolated to the same Task that issued
`*Exec`. On valid replacement, the old remainder is discarded (rather than
resumed after the new file); this follows the single-current-source policy and
is covered explicitly.

## Implementation and evidence

BASIC64 command parsing and path policy live in
[`modules/RicochetCommands.bas64`](../modules/RicochetCommands.bas64). The
bounded HostFS read, preflight, input routing, provenance, and task-owned
cursor are in [`src/swi.rs`](../src/swi.rs) and
[`src/memory.rs`](../src/memory.rs). `modules/Console.bas64` remains the
OS_ReadC/OS_ReadLine policy layer over the checked input primitive. Tests are
in [`tests/ricochet_exec.rs`](../tests/ricochet_exec.rs), with physical-line
and preflight regressions in
[`tests/ricochet_exec_diagnostics.rs`](../tests/ricochet_exec_diagnostics.rs), and adjacent
console/MOS, authorization, alias, and Obey coverage in their existing test
targets.

All source diagnostics and physical-line limits use one CR/LF boundary rule:
CRLF is one terminator, while lone CR and lone LF each terminate one line.
This is applied consistently to control/NUL preflight, invalid UTF-8 (using
the valid-byte-prefix offset), physical-line length/count checks, and
normalized runtime input. Thus an invalid byte or control after a CRLF line is
reported on the following physical line, not one line later. Tests cover LF,
CR, CRLF, mixed and blank lines, plus a final unterminated line. A rejected
replacement remains atomic: it reports the candidate's guest path and
physical line without executing a valid-looking prefix, and preserves the
previous source cursor for continued reads. The stdio regression also checks
host-path redaction and that queued healthy commands still run after the
reported error.

The independent focused command
`cargo test --no-default-features --test ricochet_exec --test mos_calls
--test ricochet_obey --test ricochet_obey_input_regressions
--test ricochet_obey_parameters --test ricochet_cli_aliases
--test ricochet_alias_boundaries --test ricochet_authorization
--test ricochet_command_help` passed all 37 tests. Exec-specific assertions
cover MOS `OS_ReadC` and `OS_ReadLine`, BASIC `INPUT`, same-Task isolation,
mixed line endings and an unterminated final line, queued-key preservation
during BASIC `INKEY`, atomic replacement and failure preservation, bare stop,
valid replacement from an Exec command line, queued stdio continuation, whole
source/line/work bounds, guest path/line errors, and host-path redaction.

For the physical-line follow-up, the independent focused command
`cargo test --no-default-features --test ricochet_exec_diagnostics
--test ricochet_exec --test mos_calls --test ricochet_obey
--test ricochet_obey_input_regressions --test ricochet_obey_parameters`
passed all 34 tests. This includes all five new diagnostic regressions and
the existing Exec tests, alongside the selected MOS/Obey input-recovery
targets.

The final independent `cargo test --no-default-features` run exited
successfully: 218 unit tests passed, four GPU-dependent tests were ignored,
and all integration/doc-test targets passed, including the five diagnostic
tests. The first full attempt had an
assertion failure in the configuration-recovery integration; that target
passed in isolation, and the subsequent complete run passed. The focused and
follow-up full runs used distinct `RICOCHET_CONFIG_PATH` and
`RICOCHET_DEMO_VOLUME` values. `rustfmt --check --edition 2024 src/swi.rs
src/memory.rs tests/ricochet_exec.rs tests/ricochet_exec_diagnostics.rs` and
`git diff --check` passed. No
formatting claim is made for BASIC64 source beyond successful capsule parsing
and the runtime tests.

## Remaining gaps

- `OS_Byte 198`, raw open-handle switching, nested Exec stacks, serial input,
  and complete native input-source selection are not implemented by this
  command subset.
- `Exec` does not support command-line arguments, options, arbitrary CLI/GSTrans
  expansion, input/output redirection, pipelines, `Run$Path`, filetype autorun,
  or automatic `!Boot` execution.
- This work does not establish full RISC OS CLI, Console, BASIC, or script
  compatibility, and does not complete the broader command or execution
  roadmap phases.
