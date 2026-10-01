# OS_FSControl compatibility follow-through

## Scope and verdict

This audit reviews three compatibility corrections in the bounded hosted
`OS_FSControl` (`&29`) subset: reason 11's return-register contract, reason
37's path-variable/path-list sources, and reasons 7/8's null-directory
catalogue header. The focused, full no-default, and relevant JIT checks listed
below pass on the audited snapshot. This is not a claim that FileSwitch is
generally PRM-compatible or that WP5.4 is complete.

The policy remains in the active `FileSwitch.FileSystemControl` BASIC64
definition. Rust mechanisms are limited to checked caller-memory/string
access, task-local filing-system state, bounded variable retrieval and
guest-path candidate lookup. OS_FSControl reasons are not reintroduced as a
Rust catch-all. The primary reference is the [RISC OS PRM, Volume 2, Chapter
27: FileSwitch](https://www.riscos.com/support/developers/prm/fileswitch.html),
especially OS_FSControl 7/8, 11/19, and 37.

## Contracts and hosted limits

### Reason 11 and reason 19

The PRM defines reason 11 as a temporary filing-system switch from a prefix
at the beginning of the caller's string. It preserves R0 and returns R1 at
the first pathname byte after a recognized filing-system prefix, R2 as -1
when no prefix is present (no selection change) or the prior filing-system
selector when switched, and R3 as a special-field pointer or zero. Reason 19
restores the temporary selection to the current filing system.

The hosted subset recognizes only `HostFS:` and `HostFS::volume...`; it does
not implement special fields or arbitrary filing systems. The special-field
marker is examined only within the filing-system prefix: `HostFS#Special:` is
rejected, while `HostFS:Dir#leaf` is accepted as a path whose post-colon bytes
are not reinterpreted as a special-field selector. A missing prefix
is a true no-op: R0/R1 are preserved, R2 becomes `0xFFFFFFFF`, R3 becomes
zero, and task selection is unchanged. A recognized prefix switches only
the calling Task's temporary selector, returns R1 advanced by the seven-byte
`HostFS:` specifier, R2 as the previous hosted selector (HostFS is 1; no
selection is 0), and R3 as zero. Unsupported prefixes/special-field forms
fail explicitly before changing selection or output registers; the service
does not fabricate a special-field pointer. Returned R1 remains a caller
logical address derived from the checked guest pointer, not a host address.
Reason 19 copies that Task's current selector to its temporary selector.

These outputs describe the hosted single-FileSwitch model, not native module
addresses or a system-global selector table. The `HostFS::volume...` syntax
is accepted only insofar as the mounted HostFS volume validates it; no
special-field interpretation is claimed.

### Reason 37

The PRM contract is R1 pathname, R2 output buffer, R3 optional path-variable
name, R4 optional comma-separated fallback path, and R5 buffer size. When R3
names an existing variable, that value takes precedence over R4; R4 is used
only when R3 is zero or the named variable does not exist. With neither
source, canonicalization uses the current directory. An explicit filing
system reference bypasses the search sources. The PRM's two-pass result is
R5 = buffer capacity minus canonical payload length `N`; N excludes NUL, so
an exact `N+1` buffer reports 1. If the buffer is too short, the result is
the signed deficit bit pattern and the output buffer is not filled.

The hosted implementation supports bounded UTF-8 path specifications and
runtime String/LiteralString variables only; it does not import host
environment variables, expand macros/GSTrans, or rescan variable values.
An existing empty variable is still authoritative and resolves as the CSD
candidate rather than falling back to R4. The bounded comma-list parser
trims ASCII spaces, treats an empty element as CSD, and accepts at most 255
source bytes, 16 candidates, and 4095 candidate bytes plus a NUL terminator
within the 4096-byte bounded string buffer.
Nonempty prefixes must end in `.` or `:`. The first existing candidate
wins; if none exists, the final attempted candidate is returned in canonical
form rather than turning a normal miss into an error. An unmatched wildcard
spelling is retained as an unresolved leaf; wildcard matching/sorting is not
implemented or claimed. Invalid guest pointers, invalid UTF-8, malformed
source syntax, unsupported variable types, and filesystem errors fail rather
than being treated as a miss or silently falling through.

An explicitly filing-system-qualified R1 pathname bypasses R3 and R4 source
retrieval entirely; tests pass invalid logical pointers for both unused
sources and verify successful canonicalization. The output span is checked in
full before any write. R5 is treated as a
full-width U32 capacity, and returned spare/deficit bits preserve the PRM
two's-complement contract. The NUL-terminated output is written only when
the full `N+1` bytes fit; failed preflight leaves caller memory unchanged.
Candidate selection/iteration is BASIC64 policy; Rust receives bounded
source selectors/indexes and supplies checked variable/path bytes or the
guest sandbox lookup mechanism, not an OS_FSControl reason dispatcher.

### Catalogue reasons 7 and 8

The PRM defines reasons 7/8 relative to the current library directory, with
null R1 meaning the library directory itself. BASIC64 now normalizes null R1
to `%` once and uses that effective guest path for both the bounded snapshot
and the printed canonical header. Reasons 5/6 continue to use CSD (`@`) for
null R1. This avoids a mismatch where entries came from `%` but the title
described `@`. Rust's snapshot mechanism receives the already-decided path,
directory kind and wildcard; it does not receive the public OS_FSControl
reason or decide reason-9 parsing policy.

Catalogues remain hosted bounded snapshots with BASIC64 formatting, not
native FileSwitch output. Existing snapshot limits and wildcard behavior
are unchanged; this audit adds no general path expansion or CLI commands.

## Evidence and validation

The relevant tests are `tests/ricochet_fscontrol_compatibility.rs` and
`tests/ricochet_fscontrol_ownership.rs`. The compatibility fixture covers:
reason 11 no-prefix no-op and R2=-1;
recognized-prefix pointer/R2/R3 outputs, reason 19 restoration, and atomic
failure on malformed/unsupported prefixes; reason 37 variable-over-R4
precedence, empty-present variable, absent-variable fallback, explicit
filesystem bypass, ordered first match, missing ordinary leaf result,
U32-size/short-buffer behavior, and no partial output; plus different CSD
and library directories showing reasons 7/8 header and entries agree.

- Independent focused run:
  `RICOCHET_CONFIG_PATH=/tmp/ricochet_fscontrol_compat_audit_config RICOCHET_DEMO_VOLUME=/tmp/ricochet_fscontrol_compat_audit_volume cargo test --no-default-features --test ricochet_fscontrol_compatibility --test ricochet_fscontrol_ownership -- --test-threads=1`
  — **2 passed**.
- Independent full run:
  `RICOCHET_CONFIG_PATH=/tmp/ricochet_fscontrol_compat_audit_full_config RICOCHET_DEMO_VOLUME=/tmp/ricochet_fscontrol_compat_audit_full_volume cargo test --no-default-features`
  — **218 library tests passed, 4 ignored; all integration and doc-test targets passed**.
- Relevant backend run:
  `RICOCHET_CONFIG_PATH=/tmp/ricochet_fscontrol_compat_audit_jit_config RICOCHET_DEMO_VOLUME=/tmp/ricochet_fscontrol_compat_audit_jit_volume cargo test --features experimental-jit --test ricochet_fscontrol_compatibility --test ricochet_fscontrol_ownership -- --test-threads=1`
  — **2 passed**.
- `rustfmt --edition 2024 --check src/swi.rs tests/ricochet_fscontrol_compatibility.rs tests/ricochet_fscontrol_ownership.rs`, `git diff --check`, and a trailing-whitespace scan of both audit files — clean.

No live Filer/UI acceptance is claimed. Other deviations and deferred
FileSwitch families remain listed in the [bounded ownership audit](ricochet-fscontrol-ownership-audit.md);
WP5.4 and Phase 5 are not complete.
