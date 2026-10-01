# OS_FSControl ownership audit

## Verdict

The bounded hosted `OS_FSControl` (`&29`) slice is now routed through the
active `FileSwitch.FileSystemControl` BASIC64 export. Its supported reasons
are **0, 1, 5–9, 11, 13, 14, 18, 19, 22, 25, 31, 33, 37, 39, 40, 43–45, and
50**. The public reason/register policy is in `modules/FileSwitch.bas64`;
Rust supplies typed HostFS, task-state, catalogue, and checked logical-memory
mechanisms. Removing the owning module fails closed rather than returning to
the former numeric Rust implementation.

The reason 11, reason 37, and reasons 7/8 follow-through is independently
validated by the focused compatibility fixture and the full no-default test
suite; see [the compatibility follow-through audit](ricochet-fscontrol-compatibility-audit.md)
for exact commands and results. This evidence covers the bounded hosted
contracts documented there, not general native FileSwitch compatibility.

This is a bounded subset, not full FileSwitch or RISC OS compatibility. The
official reference used for this review is the [RISC OS PRM, Volume 2,
Chapter 27: FileSwitch](https://www.riscos.com/support/developers/prm/fileswitch.html).

## Audited behavior

| Reasons | Hosted behavior and reviewed contract |
| --- | --- |
| 0, 1, 39, 40, 43–45 | Set current directory, library, or URD; swap current/previous; unset CSD, URD, or Lib. State belongs to the calling Task. |
| 5–9 | Catalogue/examine directory or wildcarded objects using a bounded HostFS snapshot (up to 4096 entries and 1 MiB); BASIC64 owns output formatting. Reasons 7/8 normalize null R1 to `%` for both snapshot and header; 5/6 use `@`. This is not native catalogue formatting. |
| 11, 19 | Reason 11 recognizes bounded `HostFS:` / `HostFS::volume...` prefixes and returns the PRM-style advanced caller logical pointer, prior hosted selector, and R3=0; no prefix is a no-op with R2=-1. Special-field markers within a recognized filesystem prefix are rejected, while bytes after the colon are path data. Unsupported prefixes fail atomically. Reason 19 restores temporary to current. |
| 13, 14 | Probe/select the hosted filing system by number or bounded name. Reason 13 follows the PRM terminator distinction: R2=0 also allows `#`, `:`, and `-`; nonzero R2 allows control terminators only. On success R1 is the filesystem number and R2 an opaque logical identity; on probe miss R1 is preserved and R2 is zero. Reason 14 with R1=0 clears current and temporary selection; successful selection preserves registers. |
| 18, 31 | Convert a file type to an eight-byte, space-padded text form or parse bounded text to a numeric type. Type names come from a small hosted table rather than the native service lookup mechanism. |
| 22 | Close all channels owned by the calling Task. Native reason 22 closes files across filing systems; task scoping is a deliberate hosted-isolation difference. |
| 25 | Rename a guest object while keeping its metadata sidecar paired. It rejects existing destinations, invalid paths, deletion-locked objects, and objects open in the calling Task. Open-file conflicts are not globally tracked across Tasks. |
| 33 | Write a NUL-terminated filesystem name, or an empty name for an unknown number, while preserving registers. The whole output span is checked before writes; undersized buffers fail without partial output. |
| 37 | Canonicalize using optional bounded runtime path-variable / comma-list sources with variable-over-list precedence; an explicit filesystem path bypasses search without reading R3/R4. R5 is the full-width U32 capacity and returns the signed spare/deficit bit pattern `capacity - N`, where N excludes NUL. Output is written only when the whole `N+1` span fits. Wildcard matching, macro/GSTrans expansion, and host environment lookup remain unsupported. |
| 50 | Rename the mounted HostFS volume label after validating an existing guest object. The label is shared across Tasks; their directory and filing-system selections remain task-local. |

HostFS is assigned filing-system number 1 for this hosted profile; that number
is not a classic FileSwitch table allocation. The reason-13 control-block value
is a fixed logical identity in guest address space, not a host pointer; callers
must treat it as opaque and only test whether it is zero. Guest paths and
caller buffers use checked Task-scoped logical memory. The new helpers use
bounded, checked guest reads and preflight whole caller output spans before
writing. Host paths are not accepted as a fallback path syntax, and filesystem
errors returned through these routes are guest-redacted.

## Compatibility boundary and remaining gaps

Null R1 for reason 0 resolves to the hosted URD, as the PRM specifies. Null R1
for reason 1 selects the hosted default library (`$.Library` when present),
falling back to the current directory if it is absent; the PRM describes the
filing-system default as typically `$.Library` but does not specify this
fallback. Reason 11 rejects special-field forms and supports only HostFS
prefixes, returning a logical pointer into the original caller string rather
than a native module pointer. Reason 37 supports only bounded String and
LiteralString path variables/lists, does not expand macros/GSTrans, and does
not perform wildcard matching; when no candidate exists it returns the final
ordinary attempted candidate canonically. Reason 22 scopes closure to one
Task. Static file-type lookup, HostFS number 1, the opaque logical
control-block token, and bounded catalogue formatting are hosted choices,
not claims of native module identity or formatting.

All other reason codes fail explicitly. In particular this does not add
OS_FSControl reasons 2, 4, 10, 12, 15–17, 20–21, 23–24, 26–30, 32, 35–36,
38, 41–42, 46–49, or 51 onward; it does not implement general Run$Path,
wildcard path resolution, native BASIC file-channel statements, or the
unregistered `LIB`, `EX`, and `INFO` commands. Same-Task rename locking does
not resolve cross-Task open-file conflicts. Host I/O can still fail after an
external filesystem has partly changed; the implementation does not promise
transactions across arbitrary host I/O. Live Filer/UI behavior was not
validated in this audit, and WP5.4 remains partial.

## Evidence

The focused fixtures are `tests/ricochet_fscontrol_ownership.rs` and
`tests/ricochet_fscontrol_compatibility.rs`. They cover
module ownership and fail-closed quiescence; normal/X dispatch; direct and
named BASIC SYS; directory isolation/defaults/swap/unset; reasons 13/14
terminators, selection, miss and clear; reason 11 register preservation;
reasons 18/31; reason 33 exact, short, unknown-name and checked-buffer cases;
reason 37 path-variable/list precedence, empty-present variable, explicit-path
bypass without reading invalid R3/R4 pointers, ordered lookup/final miss,
multibyte UTF-8 path bytes with byte-counted R5, exact/short/U32-maximum
capacity, and output-span rejection; reason 25 payload/sidecar rename, collision,
lock/open and sandbox cases; reason 50 validation, shared volume rename and
unchanged state on invalid input; CLI `DIR`, `CAT`, `RENAME`, `FILETYPE`, and
`HOSTFS`; guest-path redaction and missing-owner behavior.

Independent validation used unique `RICOCHET_CONFIG_PATH` and
`RICOCHET_DEMO_VOLUME` values:

- Focused compatibility and ownership tests — **2 passed**; exact commands and isolated paths are in [the compatibility follow-through audit](ricochet-fscontrol-compatibility-audit.md).
- Full `cargo test --no-default-features` — **218 library tests passed, 4 ignored; all integration and doc-test targets passed**, including both FSControl fixtures.
- The same two FSControl targets with `--features experimental-jit` — **2 passed**.
- `rustfmt --edition 2024 --check src/swi.rs tests/ricochet_fscontrol_compatibility.rs tests/ricochet_fscontrol_ownership.rs`, `git diff --check`, and a trailing-whitespace scan of both audit files — clean.

The test suite does not establish full PRM compatibility or live GUI/Filer
acceptance. See the [FileSwitch channel](ricochet-fileswitch-channel-audit.md),
[GBPB](ricochet-gbpb-ownership-audit.md), and
[OS_File path](ricochet-os-file-path-audit.md) audits for adjacent scope.
