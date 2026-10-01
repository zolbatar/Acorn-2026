# MOS OS_Byte / OS_Word ownership audit

Status: the bounded ownership migration is implemented and independently
validated at the public dispatcher boundary. This accepts the audited
`OS_Byte` (`&06`) and `OS_Word` (`&07`) slice only; it is not a claim of full
RISC OS MOS compatibility, completion of WP5/Phase 5, or live desktop
acceptance.

## Ownership and route

`modules/Mos.bas64` is the public policy owner. Its manifest publishes
`OS_Byte`/`ByteService` and `OS_Word`/`WordService`, and selects supported
reason codes, register fields, carry/result policy, and five-byte clock-block
encoding. The normal dispatcher resolves these exports through the active
module definition. When an SWI number remains published but its owner is
inactive, dispatch reports that state instead of trying an old numeric Rust
handler. The legacy numeric `OS_Byte` and `OS_Word` match arms have been
removed.

Rust's Mos implementation is deliberately a mechanism layer, not a second
reason decoder. The `Mos` capsule grants only `MosInput`, `MosClock`,
`TaskMemory`, and `RuntimeErrors`. BASIC64 invokes typed input insert/flush/
timed-read operations, separate system/interval clock chunk reads and writes,
and checked five-byte caller-memory access. Caller blocks are logical
addresses in the original Task; memory helpers preflight the complete span,
reject the reserved X-form error block, and write atomically. The clock
primitive carries chunks only; BASIC64 does the public block byte ordering.
These provider grants do not confer MOS-session rights on the calling Task:
an ordinary `Task::new` caller is covered by the regression test. The module
is a protected foundation owner, and a deliberately quiesced owner produces
an explicit inactive-owner error rather than a fallback.

All audited entry paths converge on those public endpoints:

- Numeric and named `SYS`/`X`-form calls enter the module-owned SWI registry.
- BBC `CALL &FFF4` and `CALL &FFF1` adapt A/X/Y to `OS_Byte`/`OS_Word`; the
  latter computes a checked full or split logical address. CALL leaves results
  in registers/memory; BASIC `SYS ... TO` is the register-copyback route.
- Numeric `*FX` parses a bounded form and dispatches `OS_Byte`; it does not
  call the old mechanism directly. The hosted `*FX 151,78,243` ClockSP5
  hardware-reset sequence remains an explicit no-op because there is no such
  hosted hardware state.
- With Exec input active, timed key polling returns the timeout status without
  consuming the Exec cursor or the queued host keyboard byte.

The ownership regression asserts the active `Mos` module and the live
`ModuleOwned` route metadata (`Mos.ByteService`/`Mos.WordService`), then checks
inactive-owner behavior. This is runtime route evidence; the regression does
not launch a graphical shell or claim a separate manual `*INSPECT` session.

## Accepted hosted contract

| Entry | Implemented subset | Explicit boundary |
| --- | --- | --- |
| `OS_Byte 138` | `X=0` inserts low-byte `Y` into the hosted keyboard queue; full is reported with carry. | Queue capacity is 256 in this host, not a promise about native buffer size. Other selectors fail. |
| `OS_Byte 21` | `X=0` clears the hosted keyboard queue and queued console input. | Only buffer zero is represented; unsupported buffer selectors fail. Hosted policy preserves R2 as well as R0/R1. |
| `OS_Byte 129` | `Y<128` interprets low-byte X and Y as a 16-bit centisecond timeout; returns key in R1 and status `0`, Escape `27`, or timeout `255` in R2. Hosted carry is clear for key and set for Escape/timeout. | This is timed input only. Version/matrix scan modes and `OS_Byte 198` are unsupported. The hosted carry convention is not claimed as PRM behavior. While Exec owns the Task input stream, polling times out without consuming either source. |
| `OS_Word 1/2` | Read/write the independent 40-bit centisecond system clock through exactly five little-endian bytes at full R1 logical address. | Runtime-dispatcher session clock, not CMOS real time; session state is not persistent across process restart. |
| `OS_Word 3/4` | Read/write a separate 40-bit centisecond interval counter through the same five-byte checked block. | No interval-timer event delivery or `OS_Byte 14` enable policy is implemented. |

Unknown reasons and unsupported selectors fail explicitly. No generic
hardware implementation, new OS_Byte reason, or `OS_Byte 198` stream-handle
control is added. For `*FX`, the hosted accepted syntax is one to three
comma-separated decimal or `&`-hex values; omitted fields are not a broad
compatibility claim. The one ClockSP5 triple above is preserved as a no-op.

The PRM defines OS_Byte's low-byte reason/parameter model and cautions that
top register bits are preserved where documented; the implementation masks
into BASIC64 temporaries rather than overwriting the incoming values.
The PRM timed-input form uses a 16-bit centisecond timeout, returns R2 status
for a character/Escape/timeout, and shares its character buffer with the
input stream. OS_Word clock reasons 1–4 use a five-byte little-endian block;
the system clock and interval counter advance at 100 Hz. Ricochet's stated
deviations above (queue capacity, carry policy, hosted session clock, and no
timer event delivery) are intentional and bounded.

Primary references: [PRM OS_Byte, including `*FX`](https://www.riscos.com/support/developers/prm/osbyte.html),
[PRM Character Input and OS_Byte 129](https://www.riscos.com/support/developers/prm/charinput.html),
[PRM Time and Date, OS_Word 1–4](https://www.riscos.com/support/developers/prm/timedate.html),
and [PRM OS_Word overview](https://www.riscos.com/support/developers/prm/osword.html).
The keyboard-buffer contract is documented in the PRM
[Buffers chapter](https://www.riscos.com/support/developers/prm/buffers.html).

## Evidence and limits

The focused ownership regression covers ordinary caller access, preservation
of high input register bits, queue-full carry and flush, zero-timeout input,
X-form unsupported errors, `*FX`, both CALL adapters, direct OS_Word clock
reads/writes, high-byte and modulo-40-bit wrap, system/interval independence,
full and split pointers, invalid guest-end spans and reserved-error-block
sentinels, Exec/keyboard isolation, unsupported `&198`, and inactive-owner
error/no-fallback behavior. `tests/mos_calls.rs` remains the adjacent CALL
compatibility suite.

Independently run results for this audit:

- Focused no-default-features run:
  `RICOCHET_CONFIG_PATH=/tmp/ricochet-mos-owner-focus-20260930b.configure RICOCHET_DEMO_VOLUME=/tmp/ricochet-mos-owner-focus-20260930b-volume cargo test --no-default-features --test ricochet_mos_swi_ownership --test mos_calls --test ricochet_authorization --test ricochet_exec -- --test-threads=1` — passed 17 tests (1 + 9 + 1 + 6).
- Embedded foundation-capsule boot test:
  `RICOCHET_CONFIG_PATH=/tmp/ricochet-mos-owner-boot-20260930.configure RICOCHET_DEMO_VOLUME=/tmp/ricochet-mos-owner-focus-20260930b-volume cargo test --no-default-features embedded_foundation_capsule_has_all_owners_and_startup_authority_on_system -- --nocapture` — passed.
- Experimental-JIT checks:
  `cargo test --features experimental-jit --test mos_calls --test basic_jit_strict -- --test-threads=1` — passed 20 tests (strict JIT 10, MOS CALL bridge 10), including the strict native CALL test. It completed in about 275 seconds.
- Full default-feature-free suite:
  `RICOCHET_CONFIG_PATH=/tmp/ricochet-mos-owner-full-20260930.configure RICOCHET_DEMO_VOLUME=/tmp/ricochet-mos-owner-full-20260930-volume cargo test --no-default-features` — exited 0: 218 library tests passed, 4 ignored; all integration and doc-test targets passed, including the new ownership test.
- `rustfmt --check --edition 2024 src/swi.rs src/swi/mos.rs src/boot.rs src/memory.rs tests/ricochet_mos_swi_ownership.rs` and `git diff --check -- docs/ricochet-mos-swi-ownership-audit.md` — passed.

These are automated dispatcher/source tests, not a live Wimp session or
hardware test. Remaining work includes other MOS reasons, native key scanning
and version queries, the complete OS_Byte buffer/device surface, OS_Byte 198,
interval timer events, real-time/CMOS behavior, and unrelated WP5/Phase 5
migrations. Consult [the SWI inventory](ricochet-swi-inventory.yaml),
[MOS CALL bridge contract](mos-calls.md), [architecture](ricochet-architecture.md),
and [work packages](ricochet-work-packages.md) for the broader deferred scope.
