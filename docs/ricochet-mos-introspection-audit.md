# Ricochet MOS introspection audit

Audit status: the initial MOS command slice and task-scoped source/mutation
authorization are implemented; the focused authorization matrix passes. This
is not WP5.3 or Phase 6 completion: browser parity,
per-entity observation policies, and most Phase 6 entities remain unresolved.

Historical note: this audit originally described the temporary `*RICOCHET`
command namespace from the first WP5.3 implementation. That namespace has been
removed and is neither recognized nor advertised now. The current read-only
interface is `*INSPECT`; classic module mutations are `*RMLoad`, `*RMRun`,
`*RMKill`, and conditional `*RMEnsure`. The current contract and test evidence
are recorded in `ricochet-classic-module-commands-audit.md`.

## Contract and naming

WP5.3/WP6.1 require MOS star-command access to browser introspection through
the same permission-checked query service, with matching logical identities,
relationships, generations, readable output and BASIC64-consumable results.
Inspection must not itself mutate state; management operations require
separate explicit authority. There is no browser in this batch, so its side of
the parity requirement is untested.

The PRM names the historical module list `*Modules`, maps `OS_Module` reason 1
to `*RMLoad` and reason 4 to `*RMKill`, and documents module commands as part
of the command search order. The CLI PRM says a final dot makes a prefix
abbreviation and the first matching command wins. The CLI chapter specifies
`OS_CLI` R0 as a string terminated by NUL, Linefeed, or Return; the command line
is at most 256 bytes including its terminator, and R0 is preserved on exit.
The superseded interim implementation used `*RICOCHET ...`; that user-facing
namespace has since been removed. Current inspection uses `*INSPECT`, while
mutations use the supported classic star-command routes. The hosted list
deliberately avoids RMA/workspace addresses and routes directly to
manifest-backed services. See the official [Modules
PRM](https://www.riscos.com/support/developers/prm/modules.html) and [CLI
PRM](https://www.riscos.com/support/developers/prm/cli.html).

The PRM's reason-1 `OS_Module`/`*RMLoad` loads an `&FFA` relocatable image; if
the title already exists, the duplicate module and all its instantiations are
killed before the candidate initializes. Ricochet' reason-1 path instead reads
bounded hosted BASIC64 `&064` source and attempts compatible transactional
replacement, preserving workspace and active-call generations. This is a
deliberate hosted semantic deviation, not historical `*RMLoad` compatibility.

`RicochetCommands` owns manifest `OS_CLI (&05)` in BASIC64 and now handles
read-only `*INSPECT` plus the implemented classic module command subset.
Unrelated command lines cross its `MosCommandBridge` capability to the old Rust
parser as a transitional fallback. The current fixed BASIC64 alias table
accepts any nonempty final-dot prefix when unambiguous; exact forms win and
ambiguous forms (for example, `*INSPECT MOD.` between MODULE and MODULES) are
reported as ambiguous. This remains a static table rather than PRM's
load-order-sensitive alias search.

## Verified implementation

- `modules/Boot.bas64` imports `RicochetCommands`; the boot capsule registers it
  with `MosCommandBridge`/`RuntimeErrors`, and the capsule ownership checks
  assign `OS_CLI` to `RicochetCommands`. `ModuleManager.bas64` owns the structured
  query SWIs `Ricochet_ModuleInfo (&4FF10)`, `Ricochet_ModuleLookup (&4FF12)`,
  `Ricochet_SwiInfo (&4FF13)`, `Ricochet_ModuleExport (&4FF14)`, and
  `Ricochet_DefinitionSource (&4FF15)`, plus `OS_Module (&1E)`.
- The star-command read paths call those public `Ricochet_*` SWIs; they do not
  maintain a second identity or source registry. `&4FF14` enumerates active
  manifest SWI exports. `&4FF15` resolves an active module/definition selector
  against retained parsed source; it does not reopen HostFS. Structured results
  use the caller task's checked logical-memory buffers, with bounded C strings
  and a source payload of at most 1024 bytes per chunk (1025-byte capacity
  including NUL). Source chunks preserve exact retained UTF-8 bytes, including
  CRLF/trailing newline, and stop at UTF-8 boundaries. The API reports source
  path, current source/export generation and opaque definition ID; generation
  is zero if no single published generation can be attributed. Historical
  generations cannot yet be selected, despite runtime leases retaining old
  source while old calls remain active.
- `MODULES [filter]` reports active title/version/state; `MODULE <title>` adds
  the module's SWI name/number/definition/generation rows; `SWI <name|number>`
  reports the active owner/definition/generation; `SOURCE <module>/<definition>
  [byte-offset]` displays a current retained definition block. Source selection
  supports PROC/FN (including `FN:`), private definitions and lifecycle code.
  Control bytes in source text are rendered visibly by the CLI; the structured
  source API preserves original bytes. Module identity in this first slice is
  title-based, not an exposed stable `ModuleId`.
- `LOAD`, `RELOAD`, and `DELETE` are explicit separate command verbs routed
  through `OS_Module` reason 1 or 4 over hosted `&064` source/guest paths.
  `LOAD` and `RELOAD` both call reason 1; same-title replacement behavior is
  the hosted ModuleManager contract, not a distinct lower-level `RELOAD`
  operation. Management primitives remain
  capability-gated at the ModuleManager provider boundary. Guest source that
  requests/imports `ModuleManagement` is rejected before publication.
- OS_CLI passes the original R0 logical address without masking/tag stripping,
  accepts NUL, Linefeed, and Return terminators within its checked 256-byte
  window, rejects a missing terminator, trims leading and trailing spaces, and
  preserves R0 on successful returns. Query display and structured calls do
  not mutate the module inventory or active SWI identity/generation; tests also
  verify workspace continuity across reads.

## Contract corrections verified on the current snapshot

- `ExecuteCli` stops on NUL, Linefeed, or Return, matching the PRM. Integration
  cases invoke `*INSPECT MODULES` and `*Modules` with LF/CR terminators and confirm
  the same summary and unchanged public identities.
- `ShowSource` accepts only ASCII decimal offset digits and bounds the value to
  `0..=2147483647`; it no longer uses permissive `VAL`. Integration cases
  verify malformed text, numeric-prefix text, negative input, and overflow are
  reported without emitting source. The lower-level SWI independently checks
  total length and UTF-8 boundaries.
- Numeric `SWI` selection rejects `5X`, signed-range overflow, U32 overflow,
  and overlong hex without resolving them to another SWI; these negative cases
  are now in the same focused integration test.

## Verified tests

Independent run on the current shared snapshot:

```text
RICOCHET_CONFIG_PATH=/private/tmp/ricochet-mos-introspection-audit-regressions-20260930.configure \
  cargo test --no-default-features --test ricochet_mos_introspection \
  wp51_mos_introspection_matches_read_only_queries_and_tracks_live_generations -- --exact --nocapture
1 passed, 0 failed

cargo fmt --all -- --check
passed

RICOCHET_CONFIG_PATH=/private/tmp/ricochet-mos-introspection-audit-final-module-20260930.configure \
  cargo test --no-default-features --lib \
  swi::tests::module_manager_loads_inspects_and_unloads_guest_source_modules -- --exact
1 passed, 0 failed

RICOCHET_CONFIG_PATH=/private/tmp/ricochet-authorization-agent-review-20260930-1914.configure \
  cargo test --no-default-features --test ricochet_authorization \
  ricochet_services_enforce_task_scoped_read_and_management_authority -- --exact --nocapture
1 passed, 0 failed
```

The authorization matrix covers public metadata, source-only and
management-only profiles, trusted interactive MOS access, ordinary and
same-ID spoofed tasks, direct SWIs and OS_CLI parity, nested calls, X-form
denials, unchanged output buffers/registry/workspace after denial, and allowed
reload/delete. A runtime unit test confirms the interactive shell bootstrap is
trusted while a separately spawned desktop task remains ordinary.

The end-to-end test invokes commands through `OS_CLI` and compares human
summaries to direct `&4FF10/&4FF12/&4FF13/&4FF14/&4FF15` results. It exercises
module list/detail, SWI by name and number, source (including private PROC/FN),
successive chunks/UTF-8 boundaries, raw structured ESC versus visible CLI
escaping, retained source after unlinking the HostFS file, read-only inventory/
generation/workspace invariance, compatible same-title reload, generation/source
advance, explicit delete, and post-delete absence. Negative paths cover tagged
R0, unterminated 256-byte CLI input, malformed/unknown selectors, bad logical
pointers, undersized/oversized capacities, invalid offsets, missing identities,
and a guest import of `ModuleManagement` rejected without disturbing the
active owner. It also checks leading whitespace, the `*T.` legacy `TYPE` route,
final-dot command forms, successful R0 preservation, and HELP/other legacy
fallback. `STATUS` and `CONFIGURE` are now owned by `RicochetCommands` rather
than falling through to the Rust legacy adapter.
The focused run includes regressions for OS_CLI Linefeed/Return, strict SOURCE
offset parsing, and malformed/overflow `*INSPECT SWI` arguments. No browser
exists in this checkout, so browser/MOS parity is not tested.

## Permission boundary: implemented initial policy

Active module title/version/state and exported SWI identity are explicitly
public metadata: they identify published services and reveal neither retained
source nor task state. `Ricochet_DefinitionSource`/`*INSPECT DEFINITION` require the
original caller Task's `SourceRead`; `OS_Module` reasons 1/4 and
`*RMLoad`/`*RMRun`/`*RMKill`/`*RMEnsure` require the separate `ModuleManagement`
right. The ModuleManager BASIC64 procedures request these rights before
sensitive operations, and the Rust source/mutation service handlers recheck
the same Task before returning source bytes or changing registry state. The
provider's `ModuleIntrospection`/`ModuleManagement` capability authorizes only
its declared host mechanism; it never elevates the requestor. Direct SWIs and
star commands share this authorization boundary and structured denial codes.

Host task bootstrap is explicit: `Task::new(id)` grants neither right;
`Task::trusted_mos_session(id)` grants both; source-only and manager-only
profiles are separate Rust host constructors. Grants are private Task-object
state, not derived from the public numeric ID or any BASIC64/module data.
Interactive Runtime constructors deliberately create the trusted MOS session;
`Runtime::desktop_task` and other ordinary spawned tasks use `Task::new` and
do not inherit. Code (including a loaded module's Start hook or a program run
inside the trusted session) executes with that same Task principal. This is a
task boundary, not a per-program sandbox; untrusted code requiring isolation
must run in a separate ordinary Task. Grants last for the Task object's
lifetime and host revocation is task teardown/replacement; no guest-facing
grant/revoke call exists. Broader per-entity visibility rules,
historical-source permissions, task/resource/window observation rights and
browser parity remain open. This bounded policy replaces the former all-task
private-source and mutation access, but is not a complete Phase 6 permission
graph.

## Coverage boundary and remaining work

| Entity / relationship | Initial MOS slice | Remaining work |
|---|---|---|
| Active module inventory/title/version/state | `*Modules`, `*INSPECT MODULES`, `*INSPECT MODULE`, `&4FF10/&4FF12` | Stable opaque module identity is not exposed; no lifecycle/provenance graph. |
| Module → exported SWI; SWI → owner/definition | `*INSPECT MODULE`/`*INSPECT SWI`, `&4FF13/&4FF14`, current generation | No enumeration of exported PROC/FN symbols, imports/dependencies, or other graph edges. |
| Active definition → retained source | `*INSPECT DEFINITION` (`SOURCE` alias), `&4FF15`, PROC/FN/private/lifecycle and bounded chunks | No historical-generation selection/browser; no definition enumeration independent of known selector. |
| Tasks / active calls | None in this command family | Task ownership traversal and active invocation leases/call sites. |
| Logical memory / address spaces | None | Regions, rights and module/task ownership. |
| Files / channels | None | Open handles, channel state and ownership. |
| Windows / events / handlers | None | Window/event graph, handler-to-module traversal and source. No browser exists for UI parity testing. |
| Resources, provenance, statistics | None | Resource ownership, load provenance, dependencies/callers and execution/failure statistics. |

Full RISC OS command-table ordering/aliases and `*Modules` native RMA/address
fields remain outside this cut. Classic command names are supported only by
the explicit hosted subset and retain the documented `&064`, instance, ROM,
and lifecycle deviations; this is not complete historical RMLoad/RMKill
behavior. Browser-to-MOS
equivalence and all Phase 6 graph routes still need implementation and tests.
The authorization audit and its deny/allow regression suite certify the
task-scoped rights described above. This audit certifies only the bounded
initial module/SWI/source slice above; it does not claim Phase 6 completion or
visual/UI parity.
