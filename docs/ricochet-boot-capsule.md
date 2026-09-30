# Ricochet v1 boot capsule

## Status

The initial Phase 4 capsule contained seven modules; the current v1 capsule
contains those modules plus `RicochetCommands`, the first bounded MOS-command
policy module.
This is a deterministic source-and-manifest capsule, not a compiled module
archive or a cryptographically signed package. Rust embeds the visible `.bas64`
inputs with `include_str!`; there is no separately maintained embedded binary
that can go stale. The same builder runs at startup and can emit a capsule for
inspection or alternate selection.

## Contents and encoding

The byte stream is canonical and big-endian:

| Field | Encoding |
|---|---|
| Magic | 8 bytes: `TRBOOT01` |
| Format version | `u16`, currently 1 |
| Runtime ABI | `u32` |
| Module count | `u16` |
| Module records | Sorted by case-folded module name |
| Integrity footer | CRC-32/ISO-HDLC `u32` over every preceding byte |

Each record contains a `u16`-length UTF-8 source path, a `u32`-length canonical
`RICOCHET-MANIFEST\t1` string, a `u32`-length visible BASIC64 source string, a
`u16` grant count, and sorted `u16`-length UTF-8 capability names. No timestamps,
host paths, process IDs, or nondeterministic maps are serialized. Limits are
64 modules, 4 MiB per source, 256 KiB per manifest, and 32 MiB per capsule.

Before a capsule is accepted, runtime validates checksum, format/runtime ABI,
canonical row/order encodings, UTF-8 and bounds; it reparses every module and
requires its source-derived manifest to match the serialized canonical
manifest. It rejects duplicate module/path/SWI identities, unresolved or too-old
dependencies, unresolved exported-symbol imports, dependency cycles, grants
not requested by source, missing capability grants for primitive imports, and
missing source definitions. The loader additionally resolves each imported
primitive against the private primitive registry and the host's exact
module/capability allowlist.

The host's fixed grant policy is module-specific: `Console` receives
`ConsoleInput`, `ConsoleOutput`, `RuntimeErrors`, and `GraphicsVduStream`;
`System` receives `StartupPolicy` and `SystemQueries`; `Error` receives
`ErrorDispatch`; `Memory` receives `TaskMemory` and `RuntimeErrors`; `ModuleManager` receives
`ModuleIntrospection`, `ModuleManagement`, and `RuntimeErrors`; `TaskManager`
receives `TaskQuery` and `RuntimeErrors`; `RicochetCommands` receives only
`MosCommandBridge` and `RuntimeErrors`; `Boot` receives no host grant. `Boot` imports qualified
startup functions from `System` instead of directly accessing protected
configuration or host handoff primitives. An alternate capsule cannot promote
a manifest request into host authority; a name not on the reviewed list gets
no primitive grant.

CRC-32 detects accidental corruption and truncation; it does not authenticate
an alternate capsule. The embedded source is the trusted default. An alternate
capsule is loaded only after explicit host selection (the `RICOCHET_BOOT_CAPSULE`
environment path or the recovery prompt) and remains subject to runtime ABI,
manifest, source, primitive, module-name, and capability-policy validation.
There is no signature or cryptographic package identity yet.

## Load and publication sequence

1. Construct an empty `ModuleRegistry` and its protected primitive registry.
2. Decode and validate capsule bytes directly; no FileSwitch or public SWI is
   available or called.
3. Parse/validate all source, stage all modules, then link imports and grants.
   The public SWI table remains empty at this point.
4. Publish every capsule export through one `publish_modules` transaction.
5. Run `Start` hooks in dependency order. `Boot` depends on `Console`, so its
   hook runs after Console has become active. A failing hook discards all
   foundation modules, workspaces, startup requests, and public exports before
   returning to native recovery.

The original Phase 4 capsule published fourteen manifest-owned BASIC64 SWIs.
WP5.1 added three System services (17 total); the first WP5.3 slice adds
`OS_CLI` and two ModuleManager queries, for 20 currently published exports:

| Owner | Public services | Scope in this capsule |
|---|---|---|
| `Console` | `OS_ReadLine`, `OS_WriteC`, `OS_WriteS`, `OS_Write0`, `OS_NewLine`, `OS_ReadC` | Six Console contracts; raw host bytes and graphics/VDU byte-stream policy are separate protected primitives. |
| `Error` | `OS_GenerateError` | Reads a checked caller-owned 256-byte error block and raises structured `OSError`. Normal calls use the hosted structured-error path; X calls return the caller-task error block in R0 and set V. This is a bounded X-form contract, not exhaustive RISC OS error-vector behavior. |
| `ModuleManager` | `Ricochet_ModuleInfo` (`&4FF10`), `OS_Module` (`&1E`), `Ricochet_ModuleLookup` (`&4FF12`), `Ricochet_SwiInfo` (`&4FF13`), `Ricochet_ModuleExport` (`&4FF14`), `Ricochet_DefinitionSource` (`&4FF15`) | BASIC64 owns inventory, identity/source introspection, and post-boot `OS_Module` reason policy. The bounded load/delete behavior and deliberately unsupported historical reasons are specified below. |
| `TaskManager` | `Ricochet_TaskInfo` (`&4FF11`, project extension) | Versioned query of the caller task ID, logical address-space span, and dynamic-area count; it does not create or schedule tasks. |
| `Memory` | `OS_ChangeDynamicArea`, `OS_DynamicArea` | Standard SWI numbers and principal register shapes for task-local dynamic areas, with hosted allocation limits and unsupported callback/physical-memory features documented below. |
| `System` | `OS_SWINumberToString` (`&38`), `OS_SWINumberFromString` (`&39`), `OS_ReadMonotonicTime` (`&42`) | Active-manifest SWI identity conversion and a wrapping 32-bit centisecond counter, using checked task buffers and `SystemQueries`-gated host mechanisms. |
| `RicochetCommands` | `OS_CLI` (`&05`) | BASIC64 owns read-only `*INSPECT`, the supported classic module commands (`*Modules`, `*RMLoad`, `*RMRun`, `*RMKill`, `*RMEnsure`), `*CONFIGURE`, and `*STATUS` parsing/presentation. Inspect calls shared ModuleManager queries; module changes use separately authorized `OS_Module`. Checked configuration persistence mechanisms and task-local dynamic command scratch remain Rust mechanisms. Other MOS commands use the narrow `MosCommandBridge` fallback. |

The project query extensions use ABI version `1`. `Ricochet_ModuleInfo` takes
`R0=1`, a zero-based active-module cursor in `R1`, a caller-owned name buffer
and capacity in `R2/R3`; it returns the next cursor in `R1`, `R4=1` when a
record is present (`R4=0` ends enumeration), major/minor/patch in `R5-R7`,
and lifecycle state in `R8`. Names are returned in deterministic
case-insensitive module-name order. State codes are `0=Validated`, `1=Linked`,
`2=Published`, `3=Starting`, `4=Active`, `5=Quiescing`, `6=Retired`.
`Ricochet_TaskInfo` also takes `R0=1` and returns caller task ID in `R1`, logical
address-space span in `R2`, and active dynamic-area count in `R3`. The span is
the hosted task address-space extent, not a count of currently committed bytes;
a retired dynamic-area range remains inaccessible until a later area reuses
that slot.

`Ricochet_ModuleExport` (`&4FF14`) takes `R0=1`, a caller title pointer in R1, a
zero-based cursor in R2, name/capacity in R3/R4, and definition/capacity in
R6/R7. It returns the next cursor in R2, SWI number in R5, current generation
in R8, and `R9=1` when present. Enumeration is by SWI number and includes only
active manifest-owned SWI exports.

`Ricochet_DefinitionSource` (`&4FF15`) takes `R0=1`, a NUL-terminated
`Module/Definition` selector in R1 (`FN:Name` disambiguates functions), byte
offset in R2, source buffer/capacity in R3/R4, and source-path buffer/capacity
in R7/R8. It returns bytes copied in R0, next byte offset in R2, total byte
length in R5, a current generation when one is unambiguous in R6, and the
stable `DefinitionId` low/high words in R9/R10. Source is the exact retained
definition byte slice, including line endings/trailing newline, and is emitted
in UTF-8-boundary chunks of at most 1,024 bytes plus NUL. Capacity is 2..=1025
while data remains; one byte is accepted only at EOF. Only the current active
definition is selectable. The MOS renderer escapes display controls (ESC as
`^[`) while the structured API preserves original bytes.

Both endpoints are structured BASIC64 SWIs and are also the shared query path
for `*INSPECT MODULES`, `MODULE`, `SWI`, and `DEFINITION` (`SOURCE` alias). Query endpoints are
read-only and do not invoke load/delete mechanisms. Active module/version/state
and exported SWI identities are explicitly public metadata. The retained-source
query and `DEFINITION`/`SOURCE` command require the original caller Task's
`SourceRead` authority; `OS_Module` Load/Replace/Delete and `*RMLoad`/
`*RMRun`/`*RMKill` require separate `ModuleManagement` authority. An RMEnsure
fallback tail is checked only if it reaches one of those mutations.
ModuleManager BASIC64 calls a
capability-gated authorization mechanism before sensitive work, and the Rust
service handlers independently check the same Task before reading retained
source or changing registry state. Provider-side capabilities authorize
ModuleManager's use of Rust mechanisms only; they never upgrade the caller.

Task grants are private fields on the Task object rather than a lookup by
numeric task ID. The host's interactive MOS bootstrap explicitly constructs
`Task::trusted_mos_session`; host-only source-inspector and module-manager
constructors grant only their respective right. Ordinary `Task::new` and
`Runtime::desktop_task` tasks have neither right and spawned tasks do not
inherit the parent session's authority. A BASIC64 program or module lifecycle
hook that runs within a trusted session executes under that same task principal
and therefore shares its authority; this is not a per-program sandbox. Run
untrusted code in a separate ordinary task when isolation is required. Broader
per-entity visibility rules and task/resource/window observation permissions
remain Phase 6 work. Grants last for the Task object's lifetime; host
revocation is task teardown/replacement, with no guest-visible grant/revoke
operation.

`System` supplies `Boot` with qualified functions for reading the startup
preference and requesting one of the two host handoffs, and owns the three
query SWIs above. `OS_SWINumberToString` strips bit 17 for lookup, prepends
uppercase `X` when that bit is set, writes a NUL-terminated name, and reports
the byte count excluding NUL. `OS_SWINumberFromString` reads a control/space-
terminated caller string, matches the active manifest export name exactly,
and treats only a leading uppercase `X` as the bit-17 marker. Both use checked
caller-task logical memory with a 128-byte maximum. `OS_ReadMonotonicTime`
returns low 32 bits of centiseconds elapsed since hosted runtime initialization;
it advances independently of the settable wall clock and wraps naturally.
The historical register, terminator, re-entrancy, and X-bit details are from
the [RISC OS PRM conversions chapter](https://www.riscos.com/support/developers/prm/conversions.html)
and [time/date chapter](https://www.riscos.com/support/developers/prm/timedate.html).

The identity map deliberately includes only active manifest-owned SWIs. It
does not guess names or numbers for transitional Rust-only handlers, the
`OS_WriteI` inline alias, or unknown services; those cases return structured
identity errors. Name matching is case-sensitive apart from the recognized
leading uppercase `X`, a documented scope limit relative to the historical
PRM's broader lookup behavior. Rust handler branches for the migrated numeric services remain only as
transitional fallback scaffolding for calls made when no module export is
present; a published capsule call enters its BASIC64 owner. Other Rust
numeric/named semantic handlers remain transitional Phase 5 work, not special
bootstrap routes. Boot settings and display handoff are private,
capability-protected primitives, not public SWIs.

## Boot policy

`modules/Boot.bas64` reads the saved `Language` setting by calling the qualified
`System.ReadStartupLanguage` module function. The `System` module alone imports
the private `Host.Configuration.ReadStartupLanguage`, `Host.Boot.RequestMos`,
and `Host.Boot.RequestDesktop` primitives under `StartupPolicy`. BASIC64 Boot
chooses MOS prompt for value `0` and desktop for value `3`, then calls the
corresponding `System` handoff function. Rust's `Runtime::run` does not read the
configuration or map the `Language` number to a host route; it only consumes
the typed request. `--stdio` cannot realize a graphical handoff, so it retains
the MOS recovery prompt even if the Boot module selected desktop. An explicit
`DESKTOP` MOS command remains on the transitional OS_CLI adapter and uses the
same display handoff mechanism.

## Persisted configuration recovery

Configuration errors after foundation publication do not enter the native
capsule-recovery loop. The protected startup reader treats a missing file as
first-run defaults and latches a path-redacted recovery category for malformed
or truncated data, unsupported version/schema, invalid UTF-8, unreadable
storage, or a file larger than 64 KiB. It returns the effective safe defaults
and category to BASIC64 rather than failing `Boot.Start`; `Boot.bas64` reports
the category and then follows normal Language startup. Public `*STATUS` shows
the effective values and recovery category without exposing the host path.
Boot, Status, application settings, and display startup share the same
effective store for the session. No boot/read operation changes the source.

The first successful caller-authorized `*CONFIGURE` write or `*CONFIGURE
DEFAULTS` preserves readable original bytes in a unique sibling named
`<config>.recovery-<pid>-<sequence>` before replacing the source with canonical
v3 settings. Oversized originals are copied by streaming; config reads are
bounded. If the source is still unreadable or backup/atomic save fails, the
operation is rejected, the original is not replaced, and the recovery state
remains active. If an unreadable source was removed before repair, no bytes
remain to copy; the write does not claim otherwise. This recoverability is
available only after the foundation has loaded, so it is distinct from the
restricted native prompt used for capsule validation/link/start failures.

## Native recovery

When capsule validation, ABI checking, linking, initial publication, or a
foundation `Start` fails, the host uses raw native console/display mechanisms
to report stage, module/definition where known, structured cause, capsule and
runtime ABI, and diagnostic log. The prompt accepts only:

- `R` — retry the embedded capsule;
- `A <path>` — load one explicitly selected capsule file;
- `Q` — exit.

`A` alone prompts for the path. There is no `HELP`, normal CLI, public SWI, or
guest FileSwitch access in this surface. The selected path is a native boot
input, not a guest filename.

## Build and inspect

Cargo rebuilds the executable whenever either embedded source changes. Emit and
verify a capsule without launching the runtime:

```sh
cargo run -- --write-boot-capsule /tmp/ricochet-boot.cap
cargo run -- --verify-boot-capsule /tmp/ricochet-boot.cap
```

At runtime, `RICOCHET_BOOT_CAPSULE=/path/to/capsule.cap` explicitly selects an
alternate instead of the embedded source-derived default. Omit it for the
trusted embedded path. A selected capsule cannot expand the fixed capability
allowlist.

## Deliberate service boundaries and hosted deviations

The seven-module Phase 4 set is the *initial foundation set*, not the full RISC
OS service surface; the current capsule also contains `RicochetCommands`.
`System` owns its startup bridge and these three bounded
public queries, but is not a general public System SWI module. The guest-facing
`ModuleManager` surface provides introspection plus bounded post-boot load,
compatible-immediate replacement, and delete; broader replacement/migration
classes remain unavailable.
`TaskManager` reports only caller identity and memory summary; task
creation/scheduling and cross-task control remain later work.

`RicochetCommands` owns OS_CLI routing and the inspect, classic-module, and
configuration command families. `*INSPECT MODULES [filter]`, `MODULE <title>`,
`SWI <name|number>`, and `DEFINITION <module>/<definition> [byte-offset]`
are read-only; `SOURCE` is a read-only alias. The obsolete interim
`*RICOCHET` family is not recognized or advertised. The classic module subset
is `*Modules`, `*RMLoad <guest-path>`, `*RMRun <guest-path>`,
`*RMKill <title>`, and `*RMEnsure <title> <version> [command]`. Command parsing
and output policy live in BASIC64. `*Modules` emits title/version/state but
not historical RMA/workspace pointers or fabricated instance rows. RMLoad
and RMRun accept one HostFS path to BASIC64 `&064` source; init strings are
rejected. RMRun currently equals RMLoad because no separate application entry
exists. RMKill rejects `%instantiation`. RMEnsure uses hosted numeric
`major.minor[.patch]` comparison and a bounded internal command route; an
unsatisfied version without a command raises an error. The known native
ROM/RMA commands `*RMReInit`, `*RMInsert`, `*RMTidy`, `*RMClear`, `*RMFaster`,
`*ROMModules`, and `*Unplug` return explicit unsupported diagnostics and do
not change state. Abbreviations use a fixed exact-first BASIC64 table; they do
not reproduce PRM's dynamically composed alias/module ordering. The same
module owns `*CONFIGURE [option value]`,
`*CONFIGURE DEFAULTS`, and `*STATUS [option]`, including supported values,
defaults, validation messages, and display order. `BASICProfile` is bounded to
232 allowed ASCII bytes by the public 256-byte command limit. Status is
explicitly public read-only; configuration mutation uses a separate
`ConfigurationWrite` caller-task right and never follows from a source or
module-management grant. The interactive MOS Task receives that right through
an explicit host bootstrap; ordinary/spawned Tasks do not inherit it. Rust
persists validated typed configuration atomically and applies defensive schema
checks. Both BASIC64 command families use checked temporary task-local dynamic
scratch, released after OS_CLI even on exceptional exits. OS_CLI passes all
other commands to the narrow `MosCommandBridge`; the Rust parser remains
transitional for those historical/legacy commands. The BASIC64 route accepts PRM NUL, LF, or CR line
terminators within its checked 256-byte input window and preserves R0, matching
the [PRM OS_CLI entry](https://www.riscos.com/support/developers/prm/cli.html).
The current v3 configuration schema contains six keys: `Language`, `WimpMode`
(`Mode` alias), `BASICMode`, `BASICProfile`, `BASICTarget`, and `BASICEngine`.
`Language` accepts the standard decimal, `&hex`, and `base_num` notations but
only module IDs 0/3. WimpMode accepts Auto or the bounded composite
X/Y/C-or-G selector; six sizes and eight output depths are supported, but no
physical monitor mode IDs, scaling, or refresh selectors. WimpMode alone
selects both resolution and palette: Auto is host-sized and uses full-colour
C16M/Rgb888, while fixed selectors specify both. Old
DisplayResolution/DisplayColour rows migrate from saved files; fixed pairs
become one WimpMode selector and Window becomes Auto/full-colour. In v2 files,
an explicit WimpMode wins over the retired `RicochetOutputProfile` row. That
row and WindowFurniture are dropped at migration/save. Public commands and
full configuration replacement reject all obsolete keys. The only furniture
presentation is flat, while `WindowFurnitureLayout` remains a geometry type.
The fixed top-level table accepts any nonempty final-dot prefix only when it
is unambiguous; `*I.` and `*INSPE.` route to INSPECT, while `*M.` and `*Mod.`
route to Modules. `*T.` remains with the legacy TYPE route. INSPECT's exact
`MODULE.` selects detail, exact `MODULES.` selects listing, and `MOD.` is
explicitly ambiguous. `S.` is ambiguous between SWI and SOURCE; `SO.` selects
the read-only SOURCE alias. The optional DEFINITION byte offset is strict
decimal within BASIC64's signed 32-bit range; malformed, negative, and
overflowing values are rejected. RMLoad/RMRun pass one unquoted RISC OS guest
path token to OS_Module reason 1; the HostFS sidecar guest name hides its
physical `.bas64` filename extension. Spaces, initialization strings, and
multiple RMLoad/RMRun arguments are not supported.

### Post-boot `OS_Module` subset and safe identity queries

The RISC OS PRM assigns `OS_Module` (`&1E`) reason 1 to Load and reason 4 to
Delete, preserving R0/R1 on successful return. Ricochet keeps those reason
numbers and register roles. Reasons 1 and 4 require the requestor Task's
separate `ModuleManagement` authority; the `OS_Module` BASIC64 wrapper and
the underlying Rust LoadSource/Unload mechanisms both check that same
requestor. Reason 1 accepts a caller-scoped R1 HostFS pathname
to a UTF-8 BASIC64 source file with filetype `&064`; there are no optional
initialisation parameters. Source and path sizes are bounded. The module title
must be RISC OS-style ASCII alphanumeric (case-insensitive identity); this
avoids loading a guest title that the Delete path could not name compatibly. A
guest module must target HOSTED, may depend only on already-active modules, and
may not request a host capability or import a protected primitive. It can
import qualified public definitions from declared dependencies. A new title is
staged and linked, published for `Start`, and made Active only if `Start`
succeeds; a failed Start removes the whole export set and private workspace.

When the reason-1 path names an already-active guest title (matched without
regard to case), Ricochet instead attempts a compatible-immediate source
replacement. The candidate must preserve the installed module identity/version,
dependency and symbol-import set, capabilities/primitive imports, lifecycle
hooks and replacement policy, every SWI number/name/definition/register
contract, every exported PROC/FN signature, and the complete persistent-state
and named-type schema. The service preserves the installed title spelling,
ModuleId, instance identity, SWI entry-cell IDs, and the existing workspace;
all exported SWI generations advance in one registry transaction. Active
leases continue with their prior definition/source while subsequent calls see
the candidate source. Candidate `Start` and the old `Quiesce`/`Finalise` hooks
are not run. Parse, compatibility, dependency, or publication failure leaves
the old active source, manifest, definitions, cells, and workspace unchanged.
Foundation modules cannot be replaced. This narrow class does not perform
state migration; changes requiring new authority, dependencies, public
contracts, workspace layout, or lifecycle metadata are rejected.

Reason 4 accepts an exact full module title, runs transactional Quiesce then
Finalise, and preserves the caller's R0/R1 on success. Foundation modules cannot
be unloaded, modules with active dependents cannot be unloaded, and `%`
instantiations are explicitly unsupported. A Quiesce error restores Active
state/admission; a Finalise error leaves the module quiesced with exports
inaccessible and allows Delete to retry Finalise. A successful delete retires
the module and removes its source/program identity so the title may be loaded
again with fresh private state.

Every other historical `OS_Module` reason is rejected with a structured
`UnsupportedServiceReason`: reason 0 Run; 2 Enter; 3 Reinitialise; 5–17 (RMA,
memory insertion, enumeration, extension and instantiation operations); 18
Lookup module name; and 19–20 ROM enumeration. These reasons are not silently
reinterpreted. PRM reason 12 (Extract module information) and reason 18 (Lookup
module name) both return unsafe process data: reason 12 places module base,
private word, and postfix address in R3-R5; reason 18 places module number and
instantiation in R1-R2 plus module-code pointer, private word, and postfix
pointer in R3-R5. Those values have no safe meaning in this hosted module
model. Safe alternatives are project extensions:
`Ricochet_ModuleLookup` (`&4FF12`, ABI 1) takes `R0=1` and an exact
case-insensitive title string in caller memory at R1, returning R1 found,
R2 one-based active ordinal, R3-R5 semantic version, and R6 lifecycle state.
`Ricochet_SwiInfo` (`&4FF13`, ABI 1) takes `R0=1`, active SWI number in R1, and
three caller-owned output buffer/capacity pairs in R2/R3, R4/R5, R6/R7. It
returns the active manifest SWI name, owner module, BASIC64 definition, and
generation in R8; it exposes no host pointer. Both are Ricochet extensions, not
claims of binary-compatible OS_Module outputs.

The PRM specifies a stronger destructive same-title operation: reason 1
attempts to kill the existing module first, kills all of its instantiations,
then initializes the candidate and replaces the old module entry. Ricochet'
transactional immediate replacement is a deliberate hosted deviation that
preserves the installed module on candidate rejection or failure. The hosted
loader still omits optional init parameters and native `&FFA` images. The
reference contracts are [RISC OS PRM, Modules](https://www.riscos.com/support/developers/prm/modules.html)
and [RISC OS PRM, Generating and handling errors](https://www.riscos.com/support/developers/prm/errors.html).
The PRM documents Load as accepting an `&FFA` relocatable module and Delete as
calling its finalise entry/freeing RMA storage; Ricochet deliberately substitutes
visible, constrained BASIC64 source and versioned module lifecycle. It does not
load native module images.

`Memory` implements caller-scoped `OS_ChangeDynamicArea` and `OS_DynamicArea`
over checked logical storage. It requires automatic area numbering (`R1=-1`)
and automatic placement (`R3=-1`), bounds a single area to 16 MiB and total
reserved area space per task to 32 MiB, and bounds the `R5=-1` maximum-size
request to 16 MiB. Dynamic-area numbers and addresses are task-local, not global
machine resources. Callback/workspace addresses, doubly mapped areas, physical
page requests, and reserved flags are rejected. The hosted model reserves the
maximum region but permits guest access only through the current size; shrinking
clears inaccessible bytes. Removing an area clears and tombstones its range;
the range stays inaccessible until a later dynamic area reuses the slot, and
the reported address-space span does not shrink. The reason-2 `R8` name result
is the caller's original logical address; it never exposes a pointer into a
Rust-owned string, and callers must keep that buffer valid. See the
[RISC OS PRM memory-management chapter](https://www.riscos.com/support/developers/prm/memman.html)
for the historical `OS_ChangeDynamicArea` and `OS_DynamicArea` contracts.

`Error` preserves the standard error-block input shape but translates the
checked caller block into the runtime's structured error transport. The
dispatcher recognizes numeric X bit 17 and the named `X` prefix. On success it
clears V; on failure it returns normally for X form, writes a standard four-byte
error number plus NUL-terminated message into a reserved 256-byte slot in the
calling task's logical memory, places that logical address in R0, and sets V.
`XOS_GenerateError` is the standard exception: it preserves the supplied R0
error-block address and sets V, as specified by the PRM. Error blocks are not
shared across tasks or backed by exposed Rust pointers.
Unknown numeric or named SWIs use generic code 1; structured service errors
preserve their code. Non-X calls continue to propagate the existing
`RuntimeError` to the caller. This does not implement the RISC OS error vector,
handler, reason-specific namespaces, or exhaustive register/flag behavior for
every public SWI. Rust still supplies internal
task, memory, error-transport, registry, and I/O mechanisms behind private
capability-checked primitives; this does not make those primitives public SWIs.
The remaining public SWI semantics and full management/task/system APIs are
Phase 5 migration work. All seventeen currently published exports have
source-visible manifest ownership and executable BASIC64 definitions; no
placeholder modules imply future ownership.
