# OS_GBPB ownership audit

## Verdict and scope

The bounded `OS_GBPB` (`&0C`) migration is independently validated for
reasons 1–10, including restored PRM zero-count behavior. The public definition now dispatches to
`FileSwitch.BulkTransferService`; BASIC64 owns reason selection, register and
carry results, record layout, and continuation policy. Rust supplies checked
Task-channel transfers, caller-memory validation/writes, and a bounded
Task-local HostFS catalogue snapshot. The old Rust numeric handler is not a
fallback when FileSwitch is inactive.

This accepts only the hosted reasons 1–10 subset. Reasons 11–12, `OS_File`,
`OS_FSControl`, broader FileSwitch/path policy, and full RISC OS compatibility
remain outside this change. `WP5.4` is partial, not complete.

## Hosted contract reviewed

- Reasons 1/2 write at an explicit/current file pointer; 3/4 read at an
  explicit/current pointer. The entire caller buffer span, transfer bound,
  file range, handle, and access mode are checked before seeking or changing
  data. Transfers are limited to 1 MiB. Results follow the PRM registers:
  R2 advances by bytes transferred, R3 reports remaining bytes (zero on
  writes), R4 reports the resulting position, and carry is clear for writes
  and set for a short read. Reads beyond extent with reason 3 transfer zero
  bytes and leave the channel's sequential pointer unchanged. Zero-byte
  transfers retain PRM reason-specific pointer semantics: reason 1 moves the
  pointer to explicit R4 and zero-fills/extends the file when R4 exceeds the
  current extent; reason 3 moves the pointer to R4 when R4 is at or before
  extent, but does not update it when R4 is beyond extent; reasons 2/4 retain
  the current sequential pointer. All still validate the handle, access mode,
  caller span, and file range before mutation. They return R2 unchanged,
  R3=0, R4 at the explicit position for reasons 1/3 or original current
  position for reasons 2/4, and clear carry. Successful calls clear the
  delayed EOF-error state. Validation faults leave caller memory, cursor and
  file unchanged. A host I/O failure during actual writes can still leave
  partial external effects; the implementation does not claim transactionality.
- Reasons 5–7 return the volume, current-directory, or library-directory
  byte record. The hosted boot option and directory privilege are zero. Their
  output span is validated before any byte is written.
- Reason 8 returns current-directory entries as repeated
  `<length><ASCII name>` records (no NUL terminator), preserving the PRM
  legacy format. Reasons 9/10 accept a checked guest directory string and
  optional wildcard string; reason 9 writes NUL-terminated names, while
  reason 10 writes aligned little-endian five-word headers, NUL-terminated
  names, and zero padding. The returned R3/R4/carry values support bounded
  continuation; R2 and the other documented preserved inputs remain intact.
  A reason-10 R2 buffer must be word-aligned. Reasons 8–10 stage a bounded
  catalogue snapshot (at most 4096 entries and 1 MiB), select only whole
  records that fit R5 where present, stop at the first unfit record, then
  preflight the complete output span before writing.
- Handles and snapshots are Task-local and reuse the same open-channel table
  as `OS_Find`, `OS_BGet`, `OS_BPut`, and `OS_Args`. Filesystem access uses the
  existing checked guest HostFS context and `FileSystem` capability; there is
  no host-path fallback or authority elevation. The X form uses the existing
  caller-scoped error block. The public dispatch route and inactive-module
  test confirm there is no hidden Rust reason handler after module
  deactivation.

The [RISC OS PRM FileSwitch chapter](https://www.riscos.com/support/developers/prm/fileswitch.html)
defines the reason/register layouts, transfer counts and carry behavior,
reason-3 beyond-extent cursor rule, EOF-error-state clearing on successful
GBPB calls, including reason-1 zero-count extension and reason-3's
beyond-extent pointer rule, fixed-name records, directory record
formats/alignment, and the continuation convention. The hosted subset follows
those contracts for reasons 1–10 while making its bounds explicit. Intentional
hosted differences include a fixed boot option/privilege of zero, sorted
HostFS-visible enumeration snapshots and ordinal continuation, HostFS filing
system number 1 (not a classic allocation), and explicit 1 MiB/4096-entry
limits. HostFS hides Ricochet metadata sidecars from guest listings. Unlike a
filing system that can promise all-or-error operations, host OS writes can
fail after making partial external changes; only predictable validation
failures are asserted to be side-effect-free.

## Routes and limits checked

`SYS "OS_GBPB"` and public SWI dispatch resolve to the FileSwitch module.
The ownership regression also checks FileSwitch's active definition identity,
the X-error route, shared cursor behavior with the existing channel services,
cross-Task handle rejection, and explicit failure after FileSwitch is
quiesced. The current `demo-volume/System/Filer.bas64` does not call
`OS_GBPB`: catalogue and date queries use `SYS "Ricochet_Desktop",1/3`, so
this migration does not claim that the live Filer consumes these records.
No live desktop/Filer GUI validation was performed.

Unsupported scope is deliberate: reasons 11–12, `CALL &FFD1` adaptation,
`OS_File`, `OS_FSControl`, path variables/search paths, broader directory and
filing-system semantics, and BASIC file-channel statements/functions are not
claimed here. This is not a claim of full FileSwitch compatibility or overall
Phase 5 completion.

## Independent validation

- `RICOCHET_CONFIG_PATH=/tmp/ricochet-osfile-audit-focused-20261001/configure RICOCHET_DEMO_VOLUME=/tmp/ricochet-osfile-audit-focused-20261001 cargo test --no-default-features --test ricochet_os_file_ownership --test ricochet_gbpb_ownership --test ricochet_fileswitch_channel_ownership -- --test-threads=1`
  — passed, 3/3. Its zero-length matrix covers reasons 1–4: reasons 1/3 use
  offsets before, at, and beyond EOF plus `U32_MAX`; reasons 2/4 use
  sequential positions before/at/beyond EOF and `U32_MAX`. Each checks
  result registers/carry, cursor, file extent
  and contents, caller buffer, and delayed EOF-state clearing/rearming. A
  missing handle still errors for a zero-length request without side effects.
  The same run covers R4+R3 U32 overflow, caller-span boundaries,
  cursor/file/buffer preservation, short EOF, reasons 5–10 byte layouts,
  continuation, first-record-too-large behavior, alignment, hidden metadata,
  task isolation, and no-fallback checks.
- `RICOCHET_CONFIG_PATH=/tmp/ricochet-osfile-audit-confirm-20261001/configure RICOCHET_DEMO_VOLUME=/tmp/ricochet-osfile-audit-confirm-20261001 cargo test --no-default-features --quiet -- --test-threads=1`
  — passed: 218 library tests, 4 GPU-only ignored; all integration and
  doc-test targets passed. A preceding full run exposed five stale capsule
  export-count assertions after the added OS_File owner; those were corrected
  before this passing confirmation run.
- `RICOCHET_CONFIG_PATH=/tmp/ricochet-osfile-audit-jit-20261001/configure RICOCHET_DEMO_VOLUME=/tmp/ricochet-osfile-audit-jit-20261001 cargo test --features experimental-jit --test ricochet_os_file_ownership --test ricochet_gbpb_ownership --test ricochet_fileswitch_channel_ownership --test mos_calls --test ricochet_mos_swi_ownership --test ricochet_authorization -- --test-threads=1`
  — passed, 15/15, including the Hybrid JIT MOS bridge.
- `rustfmt --edition 2024 --check src/swi.rs src/memory.rs src/filesystem.rs tests/ricochet_gbpb_ownership.rs`
  and `git diff --check` — passed.

No commits were made for this audit. The results establish the tested hosted
SWI contract, not live GUI acceptance or completion of the broader FileSwitch
work package.
