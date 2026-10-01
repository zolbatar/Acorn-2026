# OS_File ownership audit

## Verdict and scope

The existing hosted `OS_File` subset is now dispatched by the active
`FileSwitch.FileService` BASIC64 definition. BASIC64 owns reason selection,
the caller-visible register policy, metadata-field mapping, limits, path
source choice/candidate order, and unsupported-reason errors. Rust provides
checked guest-path, caller-memory, HostFS, metadata, and bounded
file-transfer mechanisms, including one-candidate catalogue/load operations.
There is no Rust numeric `OS_File` fallback when FileSwitch is inactive.

This is a bounded migration, not full RISC OS FileSwitch compatibility. The
read/search path forms for reasons 5, 12–15, and 255 are now implemented within
explicit hosted bounds: File$Path for 5/255, an R4 path string for 12/13, and
an R4 path-variable name for 14/15. Reasons 16/17 are direct/no-search forms.
BASIC64 chooses the source and owns ordered first-match policy; Rust provides
checked source retrieval and one-candidate guest-sandbox lookup/load. The
precise grammar, bounds, PRM comparison, and evidence are in the [OS_File path
audit](ricochet-os-file-path-audit.md).

## Implemented hosted subset

The active module handles reasons 0–18 and 255, with limitations below; other
reasons, including 19–24, fail explicitly. It supports
checked guest path strings, ordinary and X SWI dispatch, and named BASIC
`SYS "OS_File"` calls. Direct numeric SWI dispatch is tested; the current
BASIC parser does not accept numeric SWI syntax in `SYS`.

- Reasons 0/10 save the caller's half-open `[R4,R5)` byte span after checking
  ordering, the 1 MiB transfer ceiling, and the complete caller-data span.
  The data is staged before HostFS mutation. Reason 0 stores load and execute
  addresses; reason 10 stores a 12-bit file type. Predictable validation
  failures leave existing bytes and metadata unchanged. Host I/O failure
  during a write can still leave partial external effects.
- Reasons 1–4 update the corresponding load address, execution address, or
  attributes; 9 assigns type `&FFD` only when no type is set; 18 sets the
  12-bit file type. The target must be an existing regular file. `FileMetadata`
  has no native date-stamp field, so reasons 9/18 do not persist the PRM's
  date/time stamp. This is an explicit model limitation.
- Reasons 5/13/15/17 catalogue using the selected search source; 6 deletes a
  direct guest path;
  7/11 create or truncate an empty file; and 8 creates a directory. Reasons
  5/6 return object type and file metadata/length when found. File type,
  load/execute address, attributes, and file length are represented using the
  hosted FileMetadata/HostFS model. Reason 8 ignores the R4 suggested directory
  entry count because HostFS has no preallocation contract. Reasons 7/11
  intentionally preserve the old hosted create/truncate behavior.
- Reasons 12/14/16/255 load a bounded file into the caller's memory. The low byte
  of R3 selects the catalogue load address when nonzero; otherwise R2 is the
  requested destination. File length is checked against the 1 MiB cap and the
  whole destination span is validated before reading/writing the caller's
  memory. Data is staged before the caller buffer changes. Search source and
  direct-path behavior are described in the path audit.
- Guest paths are read from the invoking Task's logical memory, require valid
  UTF-8 and printable content, and reject wildcard `*`/`#` rather than
  partially applying an operation. HostFS canonicalization confines access to
  the selected guest volume; paths and errors do not expose host filesystem
  locations. The `FileSystem` capability grants mechanisms to the module; it
  does not grant authority over another Task's memory or caller identity.
- Destructive save/create/delete operations reject deletion-locked objects
  and same-Task files that are open through the shared FileSwitch channel
  table. Directory deletion still relies on HostFS to reject a non-empty
  directory. Open-file conflicts are currently tracked only within the
  invoking Task, not globally across Tasks; another Task's open channel does
  not prevent mutation through this service. This is a hosted cross-Task
  compatibility gap.

The [RISC OS PRM FileSwitch chapter](https://www.riscos.com/support/developers/prm/fileswitch.html)
defines the public reason map, register contracts, File$Path and R4 path
forms, wildcard policy, metadata/date-stamp behavior, and open/locked-file
constraints. The current subset deliberately does not claim Run$Path,
wildcard, timestamp, or cross-Task open-file behavior. HostFS
metadata sidecars are hidden from guest directory listings. Host I/O is not
transactional; only preflight failures are asserted to avoid effects.

## Routes, limits, and remaining work

The regression checks direct numeric dispatch ownership, named BASIC SYS,
ordinary Task access, X form, inactive-module fail-closed behavior, metadata
and file operations, caller-buffer/path validation, lock/open checks,
oversized load rejection, invalid UTF-8, wildcard rejection, and host-path
redaction. The OS_File test is environment-isolated and uses only temporary
guest volume paths. The desktop Filer currently performs catalogue/date
queries through `Ricochet_Desktop`; this audit does not claim a live Filer or
GUI workflow through OS_File. Existing `run_guest_file*` launch helpers still
use their direct checked Rust guest-file loader and are not evidence that
OS_File owns program-launch policy.

Still deferred: Run$Path/automatic execution, wildcard enumeration, native
time stamps, cross-Task open channel coordination, exact historical
error-block identity, OS_FSControl, unsupported OS_File reasons, BASIC
file-channel syntax, and broader filing system semantics. `WP5.4` remains
partial; this is not a claim of full FileSwitch or Phase 5 completion. No live
GUI acceptance was performed.

## Independent validation

- `cargo test --no-default-features --test ricochet_os_file_paths --test ricochet_os_file_ownership -- --test-threads=1` — passed 2/2 under fresh isolated `RICOCHET_CONFIG_PATH` and `RICOCHET_DEMO_VOLUME`; see the [path audit](ricochet-os-file-path-audit.md) for path-case coverage.
- `RICOCHET_CONFIG_PATH=/tmp/ricochet-osfile-audit-focused-20261001/configure RICOCHET_DEMO_VOLUME=/tmp/ricochet-osfile-audit-focused-20261001 cargo test --no-default-features --test ricochet_os_file_ownership --test ricochet_gbpb_ownership --test ricochet_fileswitch_channel_ownership -- --test-threads=1`
  — passed, 3/3. OS_File covers module identity/no-fallback, metadata,
  catalogue, save/load/create/delete, invalid and oversized spans, paths,
  same-Task locking/open channels, and ordinary caller use. The accompanying
  GBPB target verifies zero-count reasons 1–4 at EOF boundaries, registers,
  cursor, file extent/content, caller buffer, overflow, and EOF-state behavior.
- `RICOCHET_CONFIG_PATH=/tmp/ricochet-osfile-audit-jit-20261001/configure RICOCHET_DEMO_VOLUME=/tmp/ricochet-osfile-audit-jit-20261001 cargo test --features experimental-jit --test ricochet_os_file_ownership --test ricochet_gbpb_ownership --test ricochet_fileswitch_channel_ownership --test mos_calls --test ricochet_mos_swi_ownership --test ricochet_authorization -- --test-threads=1`
  — passed, 15/15, including the Hybrid JIT MOS bridge.
- `RICOCHET_CONFIG_PATH=/tmp/ricochet-osfile-audit-confirm-20261001/configure RICOCHET_DEMO_VOLUME=/tmp/ricochet-osfile-audit-confirm-20261001 cargo test --no-default-features --quiet -- --test-threads=1`
  — passed: 218 library tests, 4 GPU-only ignored; all integration and
  doc-test targets passed. The first independent full run caught five stale
  capsule/SWI count assertions after adding the OS_File export. They were
  updated, then this clean full run passed.
- `rustfmt --edition 2024 --check src/swi.rs src/filesystem.rs src/boot.rs tests/ricochet_os_file_ownership.rs tests/ricochet_gbpb_ownership.rs` and `git diff --check` — passed.

No commits were made for this audit. The test evidence establishes this bounded
hosted subset only, not full RISC OS compatibility or live desktop acceptance.
