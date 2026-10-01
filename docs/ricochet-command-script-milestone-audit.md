# Bounded command/script milestone audit

Audit snapshot: 2026-09-30. This records the bounded command/script milestone
and the final BASIC command-family cleanup. It is an acceptance audit for this
slice, not a declaration that Phase 5 or Ricochet as a whole is complete.

## Compatibility basis and migration

The official [RISC OS PRM BASIC and BASICTrans chapter](https://www.riscos.com/support/developers/prm/basic.html)
documents `*BASIC`/`*BASIC64` as interpreter-entry commands; native `*BASIC`
can load and run a named program or use `-load` to leave a loaded program in
immediate mode for editing. The [PRM CLI chapter](https://www.riscos.com/support/developers/prm/cli.html)
documents prefix matching, leading-star/space handling, alias bypass, and
native fallback attempts to `*Run` an otherwise unrecognized command. Those
native stages do not define Ricochet's more restricted registry miss or its
explicit `*RUN <guest-path>` route. No native equivalence is inferred from the
shared names.

| Retired hosted command/state | Use now | Deliberate loss or boundary |
|---|---|---|
| `*BASICLOAD <file>` followed by `*BASICRUN` | Pass a source or tokenized guest file directly to `*BASIC <path>` or `*RUN <path>`. | Load-only behavior and cached decoded tokenized-program state are removed. Ricochet does not provide the PRM `*BASIC -load` immediate-editor workflow. Repeated runs specify the path again. Tokenized decoding and direct `.bbc` execution remain. |
| `*BASICRUN` | `*BASIC <path>` or `*RUN <path>` under `BASICEngine Interpreter` (the default) or the selected engine. | No no-argument run of a previously cached program. This retires an interpreter-vs-JIT command distinction, not BASIC language `RUN` semantics. |
| `*BASICJIT [STRICT] [file]` | Set `*CONFIGURE BASICEngine Interpreter|Hybrid|Strict`, then use `*BASIC <path>` or `*RUN <path>`. | There is no one-shot engine override or built-in alias. Hybrid/Strict remain available only in `experimental-jit` builds; a build without that feature reports the unavailable engine through the canonical file route. `--benchmark-validation` is no longer public MOS syntax; it remains an internal acceptance-test option. |

`*BASIC64 [options] <file>` remains the explicit native-mode/text-default
route. No compatibility aliases for the three retired command handlers are
installed, and a fresh command registry returns its normal `Bad command`
diagnostic for each retired exact name rather than reaching a hidden Rust
fallback. Users can still define ordinary `Alias$...` variables themselves;
that is the general alias facility, not a shipped migration alias.

The load-only loss is material: PRM `*BASIC -load` retains an editable program
in the BASIC immediate environment, whereas Ricochet's retired
`BASICLOAD` only held a decoded program object for later command execution and
did not expose a corresponding guest editor path. The hosted replacement is
therefore direct load-and-run, not a claim of preserving native `-load`
semantics. Similarly, optimized Strict runs remain configurable, but the
unoptimized benchmark-validation mode is test tooling, not a user-selectable
CLI engine.

## Acceptance evidence

The new `tests/ricochet_basic_command_surface.rs` checks Help/registry absence
and `Bad command` for the retired names; continued Help entries for `BASIC`,
`BASIC64`, and `RUN`; direct source and tokenized-file runs; persistent
`BASICEngine` set/status behavior; visible Strict compilation failure; and
`BASIC64` option diagnostics. Existing compatibility/Help tests were updated
to the reduced public surface. `tests/tokenized_compatibility.rs` continues to
check the decoder corpus. The other slice evidence remains in the linked
[command/help](ricochet-command-help-audit.md),
[variable/quote](ricochet-command-variables-audit.md),
[alias](ricochet-cli-aliases-audit.md),
[Obey](ricochet-obey-audit.md) and
[Obey parameter](ricochet-obey-parameters-audit.md), and
[Exec](ricochet-exec-audit.md) audits.

Independent focused validation used isolated configuration and volume paths:

```text
RICOCHET_CONFIG_PATH=/tmp/ricochet-command-script-audit-focused-20260930.configure \
RICOCHET_DEMO_VOLUME=/tmp/ricochet-command-script-audit-focused-volume-20260930 \
cargo test --no-default-features --test ricochet_basic_command_surface \
  --test ricochet_command_help \
  --test ricochet_command_compatibility_regressions \
  --test ricochet_cli_aliases --test ricochet_alias_boundaries \
  --test ricochet_command_variables --test ricochet_variable_expansion \
  --test ricochet_variable_quote_regressions --test ricochet_quote_state_machine \
  --test ricochet_classic_module_commands --test ricochet_mos_configuration \
  --test ricochet_mos_introspection --test ricochet_authorization \
  --test ricochet_obey --test ricochet_obey_input_regressions \
  --test ricochet_obey_parameters --test ricochet_exec \
  --test ricochet_exec_diagnostics --test ricochet_configuration_recovery \
  --test ricochet_configuration_corrections \
  --test ricochet_standard_configuration --test tokenized_compatibility
```

Result: **52 passed, 0 failed**. A separate
`cargo test --features experimental-jit --test ricochet_basic_command_surface`
passed **1/1**. The focused `cargo test --features experimental-jit --test
basic_jit_strict` suite passed **10/10**, including full ClockSP5 source and
tokenized acceptance runs with zero-interpreter strict reports. These feature
tests establish the engine backend and direct configured file route remain;
they are not a GUI benchmark or a general JIT-coverage claim.

The final independent `cargo test --no-default-features` run exited 0:
**218 unit tests passed, 4 GPU-dependent tests ignored, all integration and
doc-test targets passed**. `rustfmt --check --edition 2024` passed on the
changed Rust runtime and relevant test files, and `git diff --check` passed.
The build still reports unrelated unused/dead-code warnings in
`ConfigureStore` and resource-buffer helpers; no cleanup-specific unused BASIC
command/state warning remains in this snapshot.

The full run used
`RICOCHET_CONFIG_PATH=/tmp/ricochet-command-script-audit-final-20260930-200152.configure`
and
`RICOCHET_DEMO_VOLUME=/tmp/ricochet-command-script-audit-volume-final-20260930-200152`,
a unique symlink to the repository demo volume. The strict JIT target used a
separate configuration path and the same read-only demo-volume symlink.

## Milestone verdict and remaining work

The bounded command/script slice is now acceptance-ready: Help and execution
share the live module-owned registry; variables, quoting, aliases, bounded
Obey and task-local Exec have explicit contracts and regression coverage; and
the BASIC command cleanup has direct migration tests. This is not WP5.3
completion. WP5.3 still calls for broader MOS command coverage, environment
substitution, and full Obey/Exec behavior; Phase 5 also requires the remaining
SWI, memory/task, FileSwitch, graphics/VDU, Wimp, and project-service
migrations, and removal of transitional Rust semantic dispatch.

Explicitly deferred command/script gaps include type-2 macro variables and
general GSTrans, native module load-order and command-service behavior,
Run$Path/File$Path fallback, full Obey `-v`/`-c`, automatic `!Boot`/filetype
execution, redirection/pipelines, Help paging/window scrolling, and native
`OS_Byte 198` input-handle switching. The interpreter/JIT still have bounded
language/profile support rather than general BASIC V/VI compatibility; the
feature tests do not substitute for manual GUI interaction or performance
acceptance. Phase 6 system-browser parity, Phase 7 safe live modification,
and Phase 8 persistence/desktop composition remain subsequent roadmap work.

The test target exercises configuration and a clear Strict error through the
canonical command path, and separate feature tests exercise the strict native
engine. It does not explicitly assert a successful Hybrid guest launch through
`*BASIC`/`*RUN`; that route remains a small additional end-to-end coverage
opportunity, not a failure observed in the inspected dispatch path.
