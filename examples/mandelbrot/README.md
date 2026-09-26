# Mandelbrot graphics example

[`mandelbrot.bas`](mandelbrot.bas) is copied from the RISC OS 5.22 listing in [“Mandelbrot plotters for BBC BASIC”](https://barrowbiker.wordpress.com/2016/12/25/mandelbrot-plotters-for-bbc-basic/) on Barrowbiker's Blog, posted December 25, 2016. The post says it may work on earlier RISC OS versions and invites readers to use the code; it does not state a formal license. It is readable source, not a tokenized file produced by a BBC BASIC `SAVE`.

The checked-in [`mandelbrot.bbc`](mandelbrot.bbc) is generated from that source with sequential line numbers and the project's ARM BASIC V token encoder. It runs this listing's selected C16M path: extended 32-bit `MODE`, task-local integer and string indirection, parameterized procedures and functions, the `ColourTrans_ConvertHSVToRGB` and `ColourTrans_SetGCOL` calls, per-pixel `MOVE`/`DRAW`, nested loops, and `INKEY(0)`. This demonstrates the source features used by this program, not broad BASIC V/VI or RISC OS compatibility.

Regenerate the saved-program fixture from the unchanged listing with:

```sh
python3 tools/tokenize_full_mandelbrot.py
```

Run it in the windowed app from the repository root:

```text
BASICLOAD $.mandelbrot.mandelbrot
BASICRUN
```

All three `.bbc` fixtures from this folder are also staged under `demo-volume/mandelbrot`. HostFS metadata gives them the BASIC file type and exposes extensionless guest names.

The listing asks for a 1680 × 1050 display and up to 8192 iterations per pixel, so its interpreted run can take a while. The window uses a shared RGBA raster surface and 60 Hz display updates; plot history does not grow with the number of pixels. Press a key after the image completes to return to the prompt.


## Reduced hosted example

[`reduced.bas`](reduced.bas) remains a quick project-authored derivative for standard RISC OS MODE 12 (640 × 256, 16 colours). It visits 640 × 256 pixels, caps each point at 48 iterations, uses the first eight logical colours, and avoids the original's extended mode and `ColourTrans` path.

The checked-in [`reduced.bbc`](reduced.bbc) is generated from that numbered source with:

```sh
python3 tools/tokenize_mandelbrot.py
```

The generator reuses the project's limited ARM BASIC V fixture encoder. These bytes are project-generated, not a `SAVE` from a named BBC BASIC ROM, and the example does not establish general BASIC V compatibility.

Run it in the windowed app from the repository root:

```text
BASICLOAD $.mandelbrot.reduced
BASICRUN
```

The reduced program publishes scene snapshots no more than once per 16.667 ms (about 60 Hz). The windowed app is required to see graphics; the stdio frontend runs the BASIC interpreter without a visible display.
