# Text and pixel plotting

`text-and-pixels.bas` is the readable source for a small shared-boundary BASIC
V fixture. `text-and-pixels.bbc` is the tokenised saved-program form loaded by
`BASICLOAD`. Its program selects MODE 1, prints two lines, and plots a red plus
through `GCOL` and `PLOT` on the same display.

Regenerate the fixture with:

```sh
python3 tools/tokenize_graphics_demo.py examples/graphics/text-and-pixels.bas examples/graphics/text-and-pixels.bbc
```

This encoder only covers the tokens used by this demonstration; it is not a
general BASIC V tokenizer.

In the windowed app, enter:

```text
BASICLOAD examples/graphics/text-and-pixels.bbc
BASICRUN
```

The MOS prompt, help output, program text, and graphics all use the same
640×256 framebuffer and the supplied BBC Micro 8×8 character bitmap. Run
`HELP` for the command list, and `QUIT` or close the window to exit. For the
original terminal adapter, use `cargo run -- --stdio`.
