# Bounded `*Obey` audit

Audit date: 2026-09-30.

This audit covers the initial hosted `*Obey <guest-path>` command only. It is
an independently reviewed subset of the RISC OS command-file behavior, not a
claim of full Obey or command-environment compatibility. The implementation
keeps command policy in `RicochetCommands.bas64`; Rust supplies bounded,
caller-task HostFS reads, nested source frames, line buffers, and cleanup.

## PRM contract and hosted subset

The RISC OS PRM describes Obey files as directly read command scripts. It
documents nested Obey calls, parameters `%0` through `%9` and `%*n`, setting
`Obey$Dir`, and stop-on-error behavior. Its syntax includes `-v` line echo and
`-c` caching. The PRM says recursive Obey depth is limited (historically 20,
and explicitly not a value to rely on). See [PRM Volume 4, Chapter 85:
Command scripts](https://www.riscos.com/support/developers/prm/commandscripts.html).
The CLI chapter documents `|` comment lines after leading stars and spaces
are stripped, command dispatch, and the OS_CLI NUL/LF/CR terminators. See
[PRM Volume 1, Chapter 24: The CLI](https://www.riscos.com/support/developers/prm/cli.html).

| Behavior | Hosted `*Obey` behavior | Compatibility boundary |
|---|---|---|
| File selection | One checked guest pathname, read through the caller's existing HostFS context. A quoted path may contain spaces. Host paths are not accepted as a fallback. | No Run$Path lookup, filetype auto-launch, application-directory launching, or automatic `!Boot` execution. |
| Source format | UTF-8 text; LF, CRLF, and CR line endings; blank lines; horizontal tabs; leading spaces/tabs and any leading `*` prefix before `|` comments; final unterminated line. Tabs separate command words and are passed through in arguments for command-specific validation. | No tokenized/binary script format. Invalid UTF-8, embedded NUL, and other control characters are rejected. NUL/control validation scans the complete source before its first line can execute and reports the guest path and physical line. |
| Dispatch | Each nonblank, noncomment line uses BASIC64's active command registry in sequence. Leading stars/spaces/tabs are normalized before comment recognition; pipes elsewhere in command values are preserved. The first error stops the current nested script chain and reports guest path and line. | This remains distinct from the separately audited task-scoped `*Exec` input redirection and is not general OS_CLI preprocessing. |
| Nested scripts | Supported through task-local stacked source frames; earlier completed command effects remain if a later line fails. `QUIT` unwinds active scripts and does not run later lines. | No script transaction or rollback. The hosted recursion ceiling is eight active sources, selected below the PRM's historical ceiling to fit the current interpreter stack. |
| Bounds | At most 65,536 bytes across active source files, 255 bytes per command line, and 4,096 physical lines in one nested session. | Limit errors reject the affected request; lines and files are not silently truncated. |
| Authority and cleanup | Commands retain the original caller Task and its rights. Nested errors unwind frames and their guest dynamic buffers; nested CLI cleanup preserves scratch owned by the enclosing command. | Obey does not acquire extra write, module, or host filesystem rights. |

This initial audit was written before positional parameters and `Obey$Dir`
were added. The current hosted subset supports bounded `%0`–`%9`, `%*n`, and
`%%` substitution plus a task-local, read-only `Obey$Dir`; see the separate
[parameter audit](ricochet-obey-parameters-audit.md) for the current grammar,
scope, deviations, and tests. `-v`, `-c`, general command substitution, and
filetype-based autorun remain unsupported by Obey. Bounded Exec input
redirection is a separate feature; see
[`ricochet-exec-audit.md`](ricochet-exec-audit.md). PRM references: [Volume 4, Chapter 85: Command scripts](https://www.riscos.com/support/developers/prm/commandscripts.html),
[Volume 1, Chapter 18: Conversions](https://www.riscos.com/support/developers/prm/conversions.html),
and [Volume 4, Chapter 91: System variables](https://www.riscos.com/support/developers/prm/systemvars.html).
Commands inside a script also pass through the shared bounded CLI alias
resolver; its separate limits and deviations are in the
[CLI alias audit](ricochet-cli-aliases-audit.md).

## Evidence and remaining limits

`tests/ricochet_obey.rs` covers mixed line endings, blank/comment lines, EOF
without a terminator, quoted guest paths, successful nested continuation,
nested source/line error provenance, stop-on-error with earlier effects
retained, caller-task write denial, bounded/missing/invalid inputs, path
containment and redaction, cleanup after failure, QUIT unwinding, preservation
of queued stdio input, the nesting ceiling, and the physical-line work limit.
`tests/ricochet_obey_input_regressions.rs` covers direct and script `*|`
comments (including repeated stars, spaces and tabs), tab-separated commands,
`||` pipe escapes within values, whole-source NUL preflight at the start,
middle, end, second line, and inside a comment, other control-byte rejection,
nested malformed input with retained parent effects, path/line redaction and
cleanup, and stdio continuation after a preflight error.

The two defects in this follow-up were reproduced before correction: `*|`
comment lines were dispatched as commands because comment classification
preceded leading-star normalization, and embedded NUL bytes were accepted by
UTF-8 validation then truncated at the NUL-terminated CLI boundary, allowing
the prefix to run. The command classifier now strips leading command prefixes
before checking for a comment, and the Rust file-open mechanism preflights the
whole source before publishing a frame or executing any command. A first
independent focused run also exposed a BASIC parser collision from using the
reserved identifier `TAB`; renaming it fixed the command module.

Independent verification on 2026-09-30 used isolated
`RICOCHET_CONFIG_PATH` and `RICOCHET_DEMO_VOLUME` values. The focused command
`cargo test --no-default-features --test ricochet_obey --test
ricochet_obey_input_regressions` passed all 10 tests (6 existing Obey tests
and 4 input regressions). The full `cargo test --no-default-features` run
exited successfully: 218 unit tests passed, 4 GPU-dependent tests were
ignored, and all integration and doc-test targets passed.

This work does not complete the broader command-environment phase, establish
full RISC OS CLI preprocessing or error-vector semantics, or address the
completed command-family cleanup is recorded in
[`ricochet-command-script-milestone-audit.md`](ricochet-command-script-milestone-audit.md).
