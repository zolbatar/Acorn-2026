# Desktop and Display named-service ownership audit

## Verdict

The existing project-specific `RICOCHET_DESKTOP` and `RICOCHET_DISPLAY`
services now execute their action and version policy in inspectable BASIC64
definitions. Their names route through a closed dispatcher map to
`DesktopServices::DESKTOPSERVICE` and `DisplayManager::DISPLAYSERVICE`; neither
name is assigned a numeric SWI identity. Rust retains bounded HostFS, Wimp,
configuration, and display mechanisms rather than a second semantic fallback.
This is a partial WP5.7 ownership slice, not completion of WP5.7 or Phase 5.

## Ownership and contract

The dispatch map is deliberately closed to the two names and their exact
exports. Dispatch checks that the owner is active, the export and retained
BASIC64 definition exist, and invokes that definition. The recorded route
contains the module, definition identity, and source hash. There is no numeric
alias, arbitrary module-export invocation, or Rust semantic fallback after
quiesce/retirement. X forms use the shared named-service error transport.
These are Ricochet APIs, not native RISC OS service contracts.

`DesktopServices.bas64` owns action selection, result-kind classification,
and unsupported-action errors:

- Action 1 fetches a zero-based, case-folded HostFS catalogue entry for a
  caller guest directory. It returns kind (0 end, 1 BASIC, 2 directory, 3
  text/source, 4 other), file type, and byte length. The existing Filer treats
  local BASIC filetypes `&FFB` and `&064` as executable BASIC/BASIC64 files.
- Action 2 returns the mounted volume-name length and writes its NUL-terminated
  name to the caller's guest buffer.
- Action 3 returns availability and whole Unix seconds for the indexed entry;
  unavailable or unrepresentable host timestamps are explicitly `0,0`. This
  is an additive Filer sorting helper, not the native five-byte RISC OS time
  contract.
- Action 4 registers the system-menu recipient using the original caller's
  Task identity.

The Rust mechanisms use checked caller logical memory and guest paths, reject
unsupported path controls, redact host paths, and bound catalogue enumeration
to 4,096 scanned entries. Catalogue names and volume names are fully span-
preflighted before the payload and NUL are written, so a short buffer or a
boundary fault does not leave a partial string. Desktop path inputs accept
either NUL termination for ordinary SYS callers or CR termination for BASIC
indirect strings used by the Filer; other control bytes are rejected. The
initial integration test exposed that the real Filer uses CR-terminated
buffers: NUL-only parsing made its catalogue call fail on `"$\r"`. The
mechanism now handles both forms, and the real browse/launch workflow passes.
This compatibility detail is part of the hosted boundary, not a claim that
every guest string ABI is interchangeable.

`DisplayManager.bas64` owns ABI version and action policy. Version 1 action 0
queries; action 1 applies resolution and colour together. Query/apply returns
the active enum IDs in R2/R3, active logical dimensions in R4/R5, host logical
dimensions in R6/R7, and save status in R8. IDs outside the currently
published resolution/colour tables, unsupported actions, and unsupported
versions fail before a state change. Apply checks `ConfigurationWrite` on the
original caller before the host mechanism persists configuration and changes
the active Wimp settings. A save failure returns R8=1 and leaves the active
settings unchanged; success returns the actual settings queried after apply.
The module's `DisplaySettings` import does not grant the caller write authority.

## Evidence and remaining boundaries

The source path and fixture verify active module/export/source identity, no
numeric publication, direct and X calls, module quiesce/retirement fail-closed
behavior, catalogue/end-of-list and metadata results, short/boundary output
buffer atomicity, volume name, modification-time availability, and original
Task menu registration. Display checks cover ordinary read-only query, denied
ordinary apply with unchanged configuration and active state, authorized
combined apply with persisted configuration, invalid enum IDs before effects,
and forced save failure with R8 and unchanged live settings. The actual
`$.System.Desktop` → Filer browse/launch integration test is also run; it
caught and verified the CR-termination correction and BASIC64 `&064`
classification.

Display apply is intentionally a hosted extension, not a replacement for a
historical display SWI. The desktop timestamp is host filesystem metadata,
not native RISC OS timestamp storage. Catalogue order follows the existing
HostFS mechanism; hidden metadata sidecars are filtered there. Enumeration
bounds and host I/O failures remain mechanism limits. This validation does not
exercise the native window visually, GPU rendering, HiDPI behavior, or a
manual user workflow. It does not migrate other project services or complete
WP5.7/Phase 5.

## Independent validation

All commands ran in `/Users/daryl/GitHub/Acorn-2026`. The full test run used
`RICOCHET_CONFIG_PATH=/tmp/ricochet_desktop_services_audit_full_config` and a
private copy of the checked-in demo volume at
`/tmp/ricochet_desktop_services_audit_volume_10012026`:

- `cargo test --no-default-features -- --test-threads=1` — 219 unit tests
  passed, 4 ignored, all integration and doc tests passed.
- `cargo test --no-default-features --test ricochet_desktop_services_ownership
  -- --test-threads=1` — passed 1/1.
- The real Filer browse/launch unit test
  `runtime::tests::basic64_desktop_browses_scrolls_and_launches_mounted_programs`
  — passed 1/1 after the CR-string fix.
- Adjacent Help, MOS configuration, graphics ownership, OS_File path, and Wimp
  redraw-context tests passed (5 integration targets, plus the selected Wimp
  unit test).
- The desktop owner fixture and Wimp redraw-context test also passed with
  `--features experimental-jit` (1/1 each).
- `git diff --check` — clean. No live GUI or visual acceptance was performed.

## Primary project references

- [`ricochet-design.md`](ricochet-design.md): project service contracts and
  desktop/Filer/display boundaries.
- [`ricochet-architecture.md`](ricochet-architecture.md) and
  [`basic64-system-profile.md`](basic64-system-profile.md): closed named route,
  module capabilities, and caller authority.
- [`ricochet-work-packages.md`](ricochet-work-packages.md): WP5.7 remains
  explicitly partial.
- `modules/DesktopServices.bas64`, `modules/DisplayManager.bas64`,
  `demo-volume/System/Filer.bas64`, and
  `tests/ricochet_desktop_services_ownership.rs`: implementation and exercised
  hosted behavior.
