# Acorn-2026

> **Build the computer Acorn might have built in 2026.**

Acorn-2026 is a design and implementation project for a modern, tinkerable computer environment inspired by Acorn and RISC OS. It preserves useful ideas and source-compatible interfaces while replacing historical implementation limits. It is not a full RISC OS simulator or desktop remake; its desktop keeps the Wimp interaction model and Filer conventions, with a modern visual style by default.

This project was also inspired by [pmirvine/risc-os](https://github.com/pmirvine/risc-os).

## Project status

The hosted Rust MOS prompt is available in one graphics-capable window: `HELP` displays help and returns to `*`, `DESKTOP` starts the BASIC64 desktop, and `QUIT` exits. The desktop shows the mounted HostFS volume and opens a BASIC64 Filer for browsing and launching programs. `cargo run -- --desktop-demo` remains a separate two-window Wimp test. The window manager and desktop services implement a documented first subset, not full RISC OS redraw or task scheduling; see the design brief for its boundaries. UTF-8 `.bas64` and `.bas` source and decoded `.bbc` programs use one parser output and compatibility execution engine; the optional Cranelift JIT consumes the same representation. `REM @BASIC64 MODE=HYBRID TARGET=AGON` selects the Agon graphics mode table at runtime. `BASICLOAD` accepts two observed tokenized saved-program record layouts and preserves their token bytes. `BASICRUN` supports the string echo fixture, a narrow shared-boundary legacy core, ClockSP5 program version 5.08, and the source-derived full Mandelbrot listing through its selected 32-bit extended mode and `ColourTrans` path. These fixtures exercise specific compatibility slices; they do not establish broad BBC BASIC V/VI compatibility or full Agon VDP emulation. See the [tokenized BASIC compatibility matrix](docs/tokenized-basic-compatibility.md) for evidence and gaps.

## Agon demo

The only Agon demo included is [`AgonTREE.bbc`](demo-volume/AgonTREE.bbc). Its HostFS guest name is `TREE` on the default `DemoDisk` volume, and its embedded `REM @BASIC64` directive selects `TARGET=AGON`. Load it from the MOS prompt with `BASICLOAD HostFS::DemoDisk.$.TREE`, then enter `BASICRUN`. The hosted Agon profile supplies its graphics mode table, but does not emulate the full Agon VDP or firmware.

## Start here

- [`docs/acorn-2026-design.md`](docs/acorn-2026-design.md) — architecture, compatibility goals, memory model, SWIs and modules, rendering, desktop model, roadmap, and open questions.
- [`docs/phase-0-mos-prompt-plan.md`](docs/phase-0-mos-prompt-plan.md) — the saved Phase 0 prompt contract and initial SWI catalog.
- [`docs/clocksp5-run-plan.md`](docs/clocksp5-run-plan.md) — implementation sequence and acceptance criteria for running the ClockSP5 fixture.
- [`docs/tokenized-basic-compatibility.md`](docs/tokenized-basic-compatibility.md) — decoder, token-profile, and execution coverage by saved-program evidence.
- [`examples/mandelbrot`](examples/mandelbrot) — the generated full-listing fixture and a reduced Mandelbrot run for quick graphics checks.

## Current direction

- A hosted Rust runtime provides low-level, kernel-like services; this does not begin as a bare-metal kernel.
- UTF-8 BASIC source and supported tokenized programs share the Rust compatibility interpreter; an opt-in Cranelift JIT consumes the same parsed program representation.
- BBC BASIC V/VI source semantics are a compatibility target where feasible.
- SWI names and documented calling behavior are treated as stable public contracts.
- Services are global while task address spaces are logical and isolated.
- The desktop, Filer, Wimp-like behavior, modules, and most OS policy are intended to be inspectable BASIC64 code.
- The system owns modern graphics and text shaping through host rendering facilities.

## First design work

Use the open questions in the architecture brief to settle the compatibility baseline, SWI contract, task memory model, and later host targets. Keep those decisions explicit before they become implementation assumptions.

## Run the first hosted prompt

Open the repository root (the folder containing `Cargo.toml`) in RustRover, then run:

```sh
cargo run
```

The app opens one window and displays the `*` prompt. Enter `HELP` to list the built-in commands. Enter `DESKTOP` to start the BASIC64 desktop in that same host window. The icon bar shows the mounted volume; select it to open the Filer, then open `Examples` and double-click a BASIC program. Closing the host window exits. Enter `QUIT` to exit from the MOS prompt.

## Persist BASIC execution preferences

Use MOS-style `CONFIGURE` commands to save the execution defaults for future
BASIC runs. For example, after starting with `cargo run-jit`:

```text
CONFIGURE BASICEngine Strict
STATUS BASICEngine
BASIC examples/clocksp5/ClockSP5.bas
```

`BASICMode` accepts `Auto`, `Classic`, `BASIC64`, or `Hybrid`; `BASICProfile`
accepts `Auto` or a profile name; `BASICTarget` accepts `Auto`, `Hosted`,
`RISCOS`, or `Agon`; and `BASICEngine` accepts `Interpreter`, `Hybrid`, or
`Strict`. `STATUS` with no argument shows every saved value. Use
`CONFIGURE DEFAULTS` to restore the interpreter and automatic mode, profile,
and target choices.

The settings apply to `BASIC <file>`, `RUN <file>`, `BASICRUN`, and BASIC
programs launched in the desktop. A program's `REM @BASIC64` fields override
the matching saved mode, target, or profile; the saved engine preference stays
in effect. Settings are read again for each run, so they apply to the next
program without restarting. On macOS they are stored in
`~/Library/Application Support/Acorn-2026/configure`; set `ACORN_CONFIG_PATH`
to use another file.

Use **Command+V** on macOS to paste clipboard text into the window (Control+V on other hosts). Pasted line breaks act like pressing Enter, so multiple pasted command lines run in sequence. Printable ASCII is sent to the guest input path; tabs become spaces and unsupported characters are skipped.

In the desktop, left-click is Select, middle-click is Menu, and right-click is Adjust. On a trackpad or two-button mouse, hold Option (Alt on other platforms) and left-click for Menu.

## Explore and launch

The editable desktop bootstrap and Filer policy are
[`demo-volume/System/Desktop.bas64`](demo-volume/System/Desktop.bas64) and
[`demo-volume/System/Filer.bas64`](demo-volume/System/Filer.bas64). They run as
separate BASIC64 guest tasks through the shared runtime. `ACORN_DEMO_VOLUME`
can select a different HostFS folder; the volume icon uses its mounted guest
volume name.

The Filer catalogues the selected guest directory from HostFS, displays up to
56 entries at a time as selectable RISC OS icons with directory, BASIC, and
file sprites, supports parent navigation and Wimp scrollbar movement, and
opens directories or BASIC source/tokenized files on a double-click. Its work
extent follows the current page, and entries beyond the current page use
previous/next controls. The renderer still has no guest redraw rectangles; it
keeps the display snapshot visible while the window is moved, resized, covered,
or scrolled. The desktop uses a modern, high-density shell by default while
keeping classic Wimp interaction and the existing guest call shapes. Acorn's
original Homerton outline face supplies shell and text-only guest labels where
its Latin repertoire fits. Wimp text screens keep their classic character grid
on a modern light surface; the MOS/BBC compatibility display remains pixel
based. Original RO 3.71 wallpaper and work-area textures are blended in at low
contrast, and `Wimp_CreateIconEx` lets BASIC64 callers supply their own 2× RGBA
icon art without changing `Wimp_CreateIcon`.
Texture provenance and asset limits are recorded in
[`resources/riscos-3.71/README.md`](resources/riscos-3.71/README.md). Each
volume-icon activation starts an independent Filer task at the volume root;
close each window to end that task. Unsupported file types and files that
disappear after listing get a visible, dismissible message. Each launched
program gets a task icon and a task-owned output window until it creates its
own Wimp windows or exits. Clicking an application icon brings its open
windows forward. A program's working directory is set to the directory
containing its guest file; its file paths still use that task's HostFS context.
Execution preferences and source directives use the same loader path as other
BASIC runs. `WimpAlpha` and `WimpBeta` in
[`demo-volume/Examples`](demo-volume/Examples) demonstrate shared Wimp windows.
For a headless rendering of the default Desktop and Filer, run
`cargo run -- --filer-snapshot /tmp/acorn-filer.ppm`.

The Filer does not edit or mutate files. The current Wimp content adapter paints
each task's BASIC display snapshot inside its window; guest redraw rectangles,
file operations, task stop controls, and orderly desktop exit are later work.

To keep using the terminal frontend, run `cargo run -- --stdio`.

To run the two-task desktop demo, use:

```sh
cargo run -- --desktop-demo
```

The host reads `alpha.bas64` and `beta.bas64` from the example directory each time it starts, so you can edit either BASIC program and relaunch without rebuilding Rust. Each task calls the documented Wimp SWI names with 32-bit guest addresses and handles its own `Wimp_Poll` events; the hosted runtime currently runs the two tasks on separate host threads.

To capture the same two real guest tasks and shared Wimp windows without opening a host window, run:

```sh
cargo run -- --desktop-demo-snapshot /path/to/desktop.ppm
```

This writes an 800 × 600 P6 PPM image after both demo tasks have reached their initial event loops.

To render a specimen from the original Homerton, Corpus, and Trinity ROM outlines, use:

```sh
cargo run -- --riscos-font-specimen /path/to/fonts.ppm
```

This produces a separate 800 × 600 P6 PPM sheet; the desktop does not add a guest `Font_*` SWI as part of this resource test.

To see text and plotted pixels together, enter:

```text
BASICLOAD examples/graphics/text-and-pixels.bbc
BASICRUN
```

The program selects MODE 1, prints two lines, and plots a red plus below the text. See [`examples/graphics`](examples/graphics) for source and fixture details.

To run the full 1680 × 1050 Mandelbrot listing in the windowed app, enter:

```text
BASICLOAD examples/mandelbrot/mandelbrot.bbc
BASICRUN
```

The program can take a while because it is interpreted and allows up to 8192 iterations per pixel. Press a key after it finishes to return to the prompt. Its source-derived tokenized file can be regenerated with `python3 tools/tokenize_full_mandelbrot.py`.

For a faster 640 × 256 example, enter:

```text
BASICLOAD examples/mandelbrot/reduced.bbc
BASICRUN
```

The reduced image appears progressively with display snapshots capped at about 60 Hz. See [`examples/mandelbrot`](examples/mandelbrot) for source, generation, and compatibility details.

To run the string echo example, enter:

```text
RUN examples/echo.bas64
```

The program asks for a line with `? ` and prints the entered string. UTF-8 source uses `.bas64`, `.bas`, `.txt`, or `.asc`; all four extensions enter the same execution engine. Tokenized saved programs use `.bbc`.

To run source directly through the JIT, start with `cargo run-jit` and pass a
source or tokenized file to `BASICJIT`, for example:

```text
BASICJIT examples/mandelbrot/reduced.bas
```

To run the small tokenised BASIC echo fixture, enter:

```text
BASICLOAD examples/basicv-echo/echo.bbc
BASICRUN
```

Type a line when the program shows `? `; it prints that line back and returns to the MOS prompt.

To run the shared-boundary legacy smoke fixture, enter:

```text
BASICLOAD examples/tokenized-compat/classic-core-smoke.bbc
BASICRUN
```

It prints `LEGACY` and returns to `*`. This project-generated fixture validates a deliberately small profile for leading `REM`, literal-string `PRINT`, and `END`; it is not a saved program from a named historical ROM.

To run ClockSP5, enter:

```text
BASICLOAD examples/clocksp5/ClockSP5.bbc
BASICRUN
```

It runs three workload passes, printing the benchmark sections and comparison each time, then returns to `*`. In this hosted profile, `TIME` is monotonic centiseconds and `INKEY` reads queued host keys; with no key pending, bare `INKEY` returns -256 and `INKEY(0)` returns -1. ClockSP5 follows its guarded path when no key is pending, skipping native ARM and hardware setup. Its final hardware reset command is accepted as a no-op. The displayed MHz figures compare this hosted BASIC interpreter's performance against the program's BBC B reference data; they do not measure the host processor's physical clock speed.

## Run ClockSP5 through strict native code

From the repository root, start the Cranelift-enabled runtime:

```sh
cargo run-jit
```

At the MOS prompt, run either checked-in form with benchmark validation:

```text
BASICJIT STRICT --benchmark-validation examples/clocksp5/ClockSP5.bas
BASICJIT STRICT --benchmark-validation examples/clocksp5/ClockSP5.bbc
```

Strict mode compiles the whole program before execution and reports zero
interpreter statement and expression counts. Benchmark-validation mode keeps
ClockSP5's measured loops and empty procedure calls. The hosted MHz figures are
the program's comparison against its BBC B reference data; they are not a
physical host clock reading or a strict-JIT speedup claim. See the
[`ClockSP5 run plan`](docs/clocksp5-run-plan.md) for coverage and limits.

To save strict mode as the default for every BASIC run, enter:

```text
CONFIGURE BASICEngine Strict
BASIC examples/clocksp5/ClockSP5.bas
```

`CONFIGURE DEFAULTS` restores the interpreter default.

## Try the experimental hybrid JIT

From the repository root, start the optimized UI with Cranelift support:

```sh
cargo run-jit
```

Then load either demo and enter `BASICJIT`:

```text
BASICLOAD examples/mandelbrot/reduced.bbc
BASICJIT
```

To try the source-derived full-size listing instead:

```text
BASICLOAD examples/mandelbrot/mandelbrot.bbc
BASICJIT
```

It can take substantially longer because it plots 1680 × 1050 points with a
higher iteration cap.

```text
BASICLOAD examples/clocksp5/ClockSP5.bbc
BASICJIT
```

`BASICJIT` compiles the full listing's verified Mandelbrot raster loop and
iteration math into one Cranelift frame kernel. Per-pixel ColourTrans and
`OS_Plot` effects go through checked runtime services. Mode setup and the final
key wait remain interpreted. The reduced fixture uses its verified iteration
kernel, and ClockSP5 uses its verified nested integer `REPEAT` region.
`BASICJIT` selects a one-run engine override. With default preferences,
`BASICRUN` remains the interpreter reference. The summary reports compiled
calls, rendered pixels where applicable, and elapsed native-region time; this
is a kernel measurement, not a whole-program speedup. Use the windowed UI to
see Mandelbrot. In stdio mode, the same MOS command works with
`cargo run-jit -- --stdio`. In
windowed mode, `BASICJIT` progress, timing, and runtime errors go to the host
application's stderr console, keeping the graphics display clear. BASIC
program output continues to use the emulated display.
