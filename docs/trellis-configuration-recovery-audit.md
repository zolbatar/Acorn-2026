# Trellis configuration recovery audit

Status: the bounded configuration-recovery path is implemented and the focused
recovery and shared-display tests pass. This audit verifies that path only; it
does not mark WP5.3 or broader MOS migration complete.

## Verified boundary

| Case | Observed behavior and evidence |
|---|---|
| First run | A genuinely missing file uses typed defaults without a recovery warning, does not create a file, and reports no recovery status. |
| Invalid stored data | `ConfigureStore` bounds reads to 64 KiB plus one byte, validates file type/UTF-8/version/schema, and latches typed defaults for malformed/truncated, unsupported-version, unsupported-schema, invalid-UTF-8, unreadable-storage, and oversized inputs. Duplicate/conflicting format headers are rejected. A `NotFound` result remains ordinary first-run behavior; other I/O errors are not treated as missing or malformed data. |
| Safe startup and readers | The private `System.ReadStartupLanguage` mechanism returns the effective Language and recovery category. `Boot.bas64` reports the cause and repair route before choosing a startup target; recovered Language is the default MOS value, not a partially parsed preference. `Runtime::run` keeps capsule `boot_failure` handling separate. BASIC preference reads and Wimp display startup use the same `ConfigureStore` session. The pre-handoff Wimp service now adopts/binds that store without resetting live display state. |
| Inspection and privacy | `*STATUS` reads effective defaults and a path-redacted recovery category. It does not expose partially parsed values or the host configuration path. The startup warning likewise gives no path and states that the file was not changed. |
| Repair | Only an explicit, authorized setting write or `*CONFIGURE DEFAULTS` repairs the file. The Rust boundary checks the caller Task's `ConfigurationWrite` right before reading write payloads or touching persistence; ordinary tasks are denied and do not create recovery copies. A successful repair preserves available original bytes in a unique sibling `.recovery-*` file, writes canonical v3 through a synced temporary file and atomic rename, then clears the session recovery latch. Oversized data is preserved by streaming rather than retaining it in startup memory. The returned copy-created flag is based on whether bytes were actually copied. |
| Failure behavior | Failed backup/save leaves the recovery latch and target contents in place; failed Wimp persistence does not publish a live display change. The startup instruction now promises preservation of “any recoverable original bytes,” not an unconditional backup when no original bytes remain available. |
| Legacy/current files | Valid v1, v2, and v3 files remain readable without eager rewrite. The integration fixture exercises v1 Language/display startup, v2 WimpMode precedence, and canonical v3. |
| Capsule failures | A broken capsule still enters restricted native recovery before guest CLI availability. A simultaneously damaged config does not mask that failure or cause a config write. Config recovery is post-foundation and does not broaden the native recovery surface. |

The relevant implementation is in `src/configure.rs`, `src/swi.rs`, and
`src/wimp.rs`; BASIC64 startup/status presentation is in `modules/Boot.bas64`,
`modules/System.bas64`, and `modules/TrellisCommands.bas64`. The policy and
phase notes are reconciled in the architecture, work-package, compatibility,
and SWI-inventory documents.

## Evidence run in this audit

- `ACORN_CONFIG_PATH=/private/tmp/acorn-trellis-config-recovery-audit-20260930-final.configure ACORN_DEMO_VOLUME=/private/tmp/acorn-trellis-config-recovery-audit-20260930-final-volume cargo test --no-default-features --test trellis_configuration_recovery -- --nocapture` — 1 passed. It covers missing, malformed/truncated, duplicate/mixed and unsupported headers, unsupported schema, invalid UTF-8, oversized and unreadable storage, path redaction, denied writes, safe-default BASIC/display consumers, explicit setting/reset repair, byte-equal recovery copies, failed-save rollback, v1/v2/v3 non-rewrite, and capsule-recovery precedence.
- `ACORN_CONFIG_PATH=/private/tmp/acorn-trellis-config-recovery-unit-20260930-audit2.configure cargo test --no-default-features --lib swi::tests::pre_handoff_display_apply_shares_latched_configuration_recovery -- --exact --nocapture` — 1 passed. The fixture pre-binds a store, constructs `windowed_with_desktop`, latches corrupt config, applies through `ACORN_DISPLAY`, then checks same-session configuration and desktop handoff consistency.
- `cargo fmt --all -- --check` and `git diff --check` — passed on the reviewed snapshot.

The integration run used `--no-default-features`; it verifies the safe
Interpreter default, not JIT execution. A full workspace test suite was not run
as part of this audit.

## Remaining limits

- All non-`NotFound` read failures share the public `UNREADABLE_STORAGE`
  category. The message distinguishes I/O trouble from corruption or an
  unsupported schema, but does not expose an OS error code; the integration
  test uses directory/path failures rather than a deterministic permission-
  denied fixture.
- The process-wide mutex serializes this process's readers/writers, not other
  processes editing the same file. Cross-process lost-update/race behavior is
  not covered; the repair path does preserve the bytes it can read before its
  own replacement.
- Backup creation and atomic replacement are tested for normal and blocked
  paths, but disk-full, power-loss/directory-sync durability, and very large
  streaming-backup stress are not simulated.

