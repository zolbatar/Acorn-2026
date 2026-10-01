# OS_File search-path audit

## Verdict

The former hosted `OS_File` path gap is closed for the bounded hosted syntax
below. BASIC64 `FileSwitch.FileService` selects the path source from the public
reason and owns the candidate loop and first-match decision. Rust receives a
bounded source-kind selector and checked caller addresses; it reads/parses the
source, constructs one guest candidate, and performs the sandboxed catalogue
or load operation. It does not select an OS_File reason or scan the candidates
itself. No numeric Rust `OS_File` fallback is active. The primitive descriptor
statically bounds R1 (the object string); R4 source and R2 load-destination
spans depend on the selected source/load mode, so they are checked inside the
executing mechanism using caller-scoped memory APIs rather than declared as
unconditional pointer rules.

This is not full RISC OS path-variable compatibility. `Run$Path`, automatic
execution, wildcard matching, GSTrans/type-2 macro expansion, and the remaining
OS_File reason families are still out of scope. In particular, no host
environment variable, host path, or host current directory supplies a guest
search prefix.

## PRM contract checked

The primary source is [RISC OS PRM, Volume 2, Chapter 27: FileSwitch](https://www.riscos.com/support/developers/prm/fileswitch.html):

- Its OS_File reason table maps 5 to File$Path catalogue, 13 to R4 path-string
  catalogue, 15 to R4 path-variable catalogue, and 17 to no-path catalogue;
  12/14/16/255 are the corresponding load forms (12 path string, 14 path
  variable, 16 none, 255 File$Path).
- R4 for reasons 12/13 points to a control-terminated comma-separated prefix
  list. R4 for 14/15 points to the NUL-terminated name of a path variable.
- Prefixes are tried in order, with the first matching object selected even
  when it is a directory. A directory selected by a load is an error; it is
  not skipped in favor of a later file. Search paths are bypassed when the
  object pathname has an explicit filing-system reference. An unset/null
  File$Path searches only the current directory.
- Load requires a file with read access. The low attribute byte defines
  owner-read bit 0 and public-read bit 4; either makes the file readable.
- For path variables, PRM permits prefix forms broader than this hosted
  parser, including expansion behavior and special filing-system syntax.
  Those broader forms are not inferred from the narrower implementation.

## Implemented hosted contract

| OS_File reason | Path source | Hosted behavior |
| --- | --- | --- |
| 5 | `File$Path` | Catalogue, ordered candidates |
| 12 | R4 path string | Load, ordered candidates |
| 13 | R4 path string | Catalogue, ordered candidates |
| 14 | R4 path-variable name | Load, ordered candidates |
| 15 | R4 path-variable name | Catalogue, ordered candidates |
| 16/17 | none | Direct lookup; no prefix search |
| 255 | `File$Path` | Load, ordered candidates |

For reasons 5/255, an unset or empty File$Path means the current directory only.
For reasons 12/13, a valid R4 string supplies the candidate list. For 14/15,
the exact runtime variable named by R4 is read on each operation; an absent
variable errors and an existing empty value means current directory only.
The runtime store is the Ricochet session store, not the host environment.
Only its String(0) and LiteralString(4) values are accepted. The path resolver
uses their current stored values and does not perform another macro/GSTrans
pass.

The bounded grammar accepts a UTF-8 path string of at most 255 bytes, terminated
by any C0 control byte or DEL. Ordinary spaces remain part of the list and are
trimmed around each comma-separated element. There may be at most 16 elements;
an empty element denotes the current directory. A nonempty prefix must end in
`.` or `:`. Wildcards and unsupported prefix syntax are rejected rather than
partially interpreted. Each constructed prefix-plus-object candidate is at
most 4096 UTF-8 bytes. A filesystem-qualified object name bypasses the search
list. This hosted check recognizes the supported HostFS qualification syntax;
it does not promise the PRM's alternate filing-system spellings.

BASIC64 advances to the next candidate only when catalogue lookup reports
NotFound. A directory is a found object and therefore wins. Invalid pointers,
invalid UTF-8, malformed prefixes, unsupported types, access denial, and other
filesystem errors are errors, not misses. A load checks read permission
(`attributes & 0x11 != 0`, owner-read or public-read) and rejects a directory;
it does not fall through on either result. Legacy files without metadata
sidecars retain the hosted default readable behavior. Loads remain capped at
1 MiB and validate the entire guest destination span before changing caller
memory. Errors are guest-path redacted. The candidate index is limited to the
16-entry bound, and this path work allocates no retained dynamic scratch.

Intentional hosted deviations and omissions:

- Prefixes containing `*` or `#`, type-2 macros, full GSTrans, and broader
  filing-system-prefix syntaxes are not supported.
- The hosted parser requires every nonempty prefix to end in `.` or `:` and
  caps source/candidate size and list length; these are explicit resource
  limits, not claims about native limits.
- Only File$Path and the explicit R4 path-string/path-variable forms above are
  implemented. Run$Path, OS_File 20–24, wildcard search, timestamps, and
  automatic run/load aliases remain deferred.
- Direct guest path behavior and HostFS metadata remain hosted mechanisms;
  this does not claim native filing-system search order or error-block identity.

## Regression evidence

`tests/ricochet_os_file_paths.rs::os_file_search_reasons_honor_path_string_variable_and_files_path`
uses one isolated HostFS volume and proves the path forms using same-named
files in separate directories. It covers first-match order, NotFound fallback,
directory first-match without skipping, reasons 12/13/14/15/5/255, reason
16/17 no-search, String(0)/LiteralString(4) path variables, variable updates
visible on the next call, unset/empty CSD behavior, and fully-qualified bypass
of both R4 and File$Path. It also checks malformed/unterminated pointers,
invalid UTF-8, malformed prefixes, combined candidate limit, no partial load
buffer writes, access-denied first match, path redaction, and dynamic-area
cleanup. The adjacent OS_File ownership target covers the existing direct
caller and mutation contracts.

## Remaining scope

`OS_File` remains a bounded subset: reasons 19–24 are not provided; reasons
13–15 are no longer silently ignored but implement only the path grammar above.
Wildcard enumeration, Run$Path/automatic execution, native timestamp
semantics, exact historical error blocks, cross-Task open-file coordination,
`OS_FSControl`, BASIC file-channel syntax, and live Filer/GUI acceptance remain
open. `WP5.4` remains partial. These path tests do not establish full RISC OS
compatibility or completion of Phase 5.

## Independent validation

Commands run from `/Users/daryl/GitHub/Acorn-2026`; each used a fresh
`mktemp -d` root for `RICOCHET_CONFIG_PATH` and `RICOCHET_DEMO_VOLUME`:

- `cargo test --no-default-features --test ricochet_os_file_paths --test ricochet_os_file_ownership -- --test-threads=1` — passed 2/2.
- `cargo test --features experimental-jit --test ricochet_os_file_paths --test ricochet_os_file_ownership --test ricochet_fileswitch_channel_ownership --test ricochet_gbpb_ownership --test mos_calls --test ricochet_mos_swi_ownership --test ricochet_authorization -- --test-threads=1` — passed 16/16, including the Hybrid JIT MOS bridge.
- Full `cargo test --no-default-features --quiet -- --test-threads=1` — passed: 218 library tests, 4 ignored, and every integration/doc-test target.
- `rustfmt --edition 2024 --check src/swi.rs src/filesystem.rs src/boot.rs tests/ricochet_os_file_paths.rs tests/ricochet_os_file_ownership.rs` and `git diff --check` — passed.

The focused and full commands above used distinct `mktemp -d` roots for
`RICOCHET_CONFIG_PATH` and `RICOCHET_DEMO_VOLUME`; the experimental-JIT run
used its own root. No test mutated a shared repository volume.

No commits were made. Passing tests establish this hosted implementation and
the enumerated cases only; no live GUI/Filer acceptance was performed.
