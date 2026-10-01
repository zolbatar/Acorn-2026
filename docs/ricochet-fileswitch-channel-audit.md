# FileSwitch channel SWI ownership audit

## Verdict and scope

The bounded ownership migration for `OS_Find` (`&0D`), `OS_BGet` (`&0A`),
`OS_BPut` (`&0B`), and the hosted `OS_Args` subset (`&09`) is independently
validated. The public SWIs dispatch to the active `FileSwitch` BASIC64 module;
the module chooses reasons, modes, registers, carry, and errors, while Rust
provides only task-scoped checked channel, filesystem, and caller-memory
mechanisms. The old numeric handlers are no longer reachable as fallback.

This accepts only this channel boundary. The later bounded `OS_GBPB` reasons
1–10 migration is recorded separately in
[`ricochet-gbpb-ownership-audit.md`](ricochet-gbpb-ownership-audit.md). Neither
audit completes WP5.4 or claims general FileSwitch or RISC OS compatibility.
`OS_File`, `OS_FSControl`, and higher-level filing policy remain outside this
ownership change.

## Contract reviewed

- `OS_Find` closes one handle, or closes only the caller Task's handles for
  handle zero. Open supports `&40` (existing read-only), `&80` (create or
  truncate, read/write), and `&C0` (existing read/write). Bit 3 requests an
  error rather than handle zero for a missing existing file. Path-selector
  bits 0–1 and reserved bits 4–5 are rejected; the hosted path is the checked
  guest pathname in R1. Bit 2 is inert for files; directory opens are rejected
  by HostFS. R1/R2 remain preserved for opens. The caller's other tasks' files
  are not affected by close-all.
- `OS_BGet` takes the handle in R1 and returns the byte in R0 with carry clear.
  At EOF, the first attempt sets carry; the next attempt returns an EOF error.
  `OS_BPut` writes the low byte of R0 to the R1 channel, advances the shared
  cursor, clears the EOF-next-read state, and preserves the public registers.
- `OS_Args` supports reasons 0–5 and 7. Reason 0 with handle zero reports
  filing-system number 1; with a handle, it reads the sequential pointer.
  Reasons 1, 2, 3, and 5 set/read pointer, read extent, set extent, and read
  EOF status. Reason 4 returns extent as a hosted substitute for allocated
  size (HostFS has no separate allocation-size model). Reason 7 returns the
  canonical guest name with NUL and PRM two-pass R5 spare/deficit semantics;
  the complete name-plus-terminator write is bounds-checked before writing.
  Reasons 6, 8, 254, 255, and other unsupported reasons fail explicitly.
- Channel handles and file cursors are Task-owned. A handle value from another
  Task cannot address that Task's channel; `OS_GBPB` sees the same channel and
  cursor when called by the owning Task. Path resolution is through the
  existing guest HostFS sandbox only: no host pathname fallback or added file
  authority. Invalid UTF-8 paths fail rather than being lossily rewritten.
- The `SYS` and BBC `CALL &FFCE`/`&FFD4`/`&FFD7` bridges enter the same public
  FileSwitch SWIs. The historical BASIC `OPENIN`, `OPENOUT`, `OPENUP`,
  `BGET#`, `BPUT#`, `PTR#`, `EXT#`, and `EOF#` language facilities are not
  implemented by this runtime and are not claimed or tested as routes into
  these channels.

## PRM comparison

The [RISC OS PRM FileSwitch chapter](https://www.riscos.com/support/developers/prm/fileswitch.html)
defines `OS_Find` open modes `&4X`, `&8X`, and `&CX`, bit 3's missing-file
behavior, and bits 0–1 path selection. The hosted subset keeps the open-mode
and missing-file behavior, but omits path strings/path variables and rejects
their selector bits. It also scopes `OS_Find 0` close-all to one Task instead
of exposing the globally risky close-all behavior the PRM warns against in a
multitasking environment. HostFS does not open directories as byte streams.

The PRM's `OS_BGet` contract is R1 handle, R0 byte when carry is clear, one
carry-set EOF attempt, then an EOF error; `OS_BPut` takes R0 byte/R1 handle,
preserves registers, advances the pointer, and clears the EOF-error-next-read
state. This matches the hosted implementation. The PRM lists `OS_Args 4` as
allocated size, which this hosted volume cannot distinguish from extent. Its
`OS_Args 7` two-pass convention says pass one returns negative canonical-name
length, pass two needs `1-R5` bytes including NUL, and R5 reports spare bytes
including the terminator (or a signed deficit). The hosted contract follows
that negotiation and writes no partial name when the full output does not
fit.

## Ownership and safety evidence

The `FileSwitch` module's reviewed host grants are `FileSystem` and
`RuntimeErrors`. Its primitive calls perform checked guest-string reads,
guest-path resolution, per-task handle operations, checked pointer/extent
operations, and a full checked caller-memory write for the canonical name.
The open mechanism reserves a task handle slot before creating or truncating a
host file, avoiding file side effects when the task cannot retain the channel.
Close-all and handle lookup operate on the invoking Task's table. The focused
test confirms `OS_GBPB` cursor sharing, cross-Task isolation, X-form failure,
no Rust fallback after module quiescence, and error paths without host-volume
path disclosure.

The `OS_Args 7` output bit pattern requires a narrow signed-result conversion
because BASIC's ordinary `%` numeric binding is signed 32-bit while the SWI
register is U32. BASIC64 still chooses reason 7, whether the buffer should be
written, and the capacity/length calculation; the helper only computes the
unsigned spare/deficit result. Boundary tests cover exact fit, short buffers,
U32-maximum capacity, and caller-memory boundary rejection.

## Validation performed

Independent runs on the reviewed source tree:

- `cargo test --no-default-features --test ricochet_fileswitch_channel_ownership -- --nocapture` — passed, 1/1.
- `cargo test --no-default-features --test ricochet_fileswitch_channel_ownership --test mos_calls --test ricochet_mos_swi_ownership --test ricochet_authorization -- --test-threads=1` — passed, 12/12.
- `RICOCHET_CONFIG_PATH=/tmp/ricochet-fileswitch-final2-full-config-20260930 RICOCHET_DEMO_VOLUME=/tmp/ricochet-fileswitch-full-volume-20260930 cargo test --no-default-features -- --test-threads=1` — passed: 218 library tests, 4 GPU-only ignored; all integration and doc-test targets passed, including the Help snapshot with FileSwitch active and the final `&44` regression.
- `RICOCHET_CONFIG_PATH=/tmp/ricochet-fileswitch-jit-config-20260930 RICOCHET_DEMO_VOLUME=/tmp/ricochet-fileswitch-full-volume-20260930 cargo test --features experimental-jit --test mos_calls --test ricochet_fileswitch_channel_ownership -- --test-threads=1` — passed, 11/11 (including the JIT MOS bridge case).
- Scoped `rustfmt --check --edition 2024` for the touched Rust runtime, boot, MOS bridge, FileSwitch, MOS-ownership, and Help test files; `git diff --check` — passed.

No live desktop/Filer interaction was performed for this SWI ownership audit.
Passing tests establish the checked hosted channel contract, not visual or
interactive GUI acceptance.

## Remaining work

FileSwitch path-string/path-variable selectors, `File$Path`/`Run$Path`,
directory channels, allocated-size semantics, the wider `OS_Args` reason set,
and `OS_File`/`OS_FSControl` policy migration remain separate work. `OS_GBPB`
reasons 1–10 have since moved to BASIC64; the separate audit records their
bounded transfer/catalogue subset.
The BASIC file-channel language statements/functions listed above also remain
unsupported. These are explicit scope limits, not evidence that WP5.4 or the
larger Ricochet roadmap is complete.
