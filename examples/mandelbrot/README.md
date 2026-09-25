# Mandelbrot graphics example

[`mandelbrot.bas`](mandelbrot.bas) is copied from the RISC OS 5.22 listing in [“Mandelbrot plotters for BBC BASIC”](https://barrowbiker.wordpress.com/2016/12/25/mandelbrot-plotters-for-bbc-basic/) on Barrowbiker's Blog, posted December 25, 2016. The post says it may work on earlier RISC OS versions and invites readers to use the code; it does not state a formal license. It is kept as a prospective BBC BASIC V/VI RISC OS graphics integration case, but has not been independently run or confirmed against a named interpreter release. This is readable text, not a tokenized file produced by a BBC BASIC `SAVE`.

The program exercises extended `MODE` blocks, `SYS` calls to `ColourTrans`, HSV-to-RGB conversion, palette setup, integer and string memory buffers, per-pixel `MOVE`/`DRAW`, numeric functions, nested loops, and `INKEY(0)`. It asks for a 1680 × 1050 display and up to 8192 Mandelbrot iterations per pixel.


## Reduced hosted example

[`reduced.bas`](reduced.bas) is a small project-authored derivative for the currently hosted MODE 2 graphics path. It visits the renderer's 640 × 256 pixels, caps each point at 48 iterations, uses the existing eight logical colors, and plots one point per pixel. It avoids the original's extended MODE block, `ColourTrans`, RGB/HSV palette, and keyboard wait. The original listing remains unchanged as the future compatibility and performance case.

The checked-in [`reduced.bbc`](reduced.bbc) is generated from that numbered source with:

```sh
python3 tools/tokenize_mandelbrot.py
```

The generator reuses the project's limited ARM BASIC V fixture encoder. These bytes are project-generated, not a `SAVE` from a named BBC BASIC ROM, and the example does not establish general BASIC V compatibility.

Run it in the windowed app from the repository root:

```text
BASICLOAD examples/mandelbrot/reduced.bbc
BASICRUN
```

While `BASICRUN` plots, the runtime publishes accumulated graphics snapshots no more than once per 16.667 ms (about 60 Hz), then sends a final snapshot when execution ends. This bounds display-update traffic while the image is being drawn; it does not reduce the interpreter's pixel or iteration work. Use the windowed app to see graphics; the stdio frontend can confirm that the fixture runs but cannot display the rendered image.
