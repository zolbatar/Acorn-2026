# Desktop flat icons

Each icon has an SVG source and a matching 1024 × 1024 RGBA PNG export for the approved modern desktop appearance in
[`docs/desired-desktop-look.md`](../../../docs/desired-desktop-look.md).

Base each icon as closely as practical on its original RISC OS 3.11 sprite,
using the [supplied desktop screenshot](../../../docs/assets/risc-os-3.11-icon-reference.png)
as a reference. Use strictly flat, front-facing geometric forms, limited colour
fills, very slight corner rounding, and consistent solid dark outlines. Do not use perspective, visible
thickness, bevels, gradients, glossy highlights, texture, cast shadows, or
skeuomorphic material rendering. Flat Remix can inform colour and polish, but
the RISC OS silhouette and details take priority.

Keep transparent canvas margins so icons remain legible at desktop sizes.
These icons are separate from the earlier glossy branding masters.
Device icons use mostly white and neutral grey casing with dark details; reserve
saturated colour for small indicators, as in the Flat Remix device reference.

| SVG source | PNG export | Subject |
| --- | --- | --- |
| `floppy-v1.svg` | `floppy-v1-1024.png` | Earlier flat drive drawing, retained as the floppy icon |
| `harddisk-v1.svg` | `harddisk-v1-1024.png` | Second RISC OS 3.11 icon-bar device: pale face, five black vents, red status light |
| `harddisk-v2.svg` | `harddisk-v2-1024.png` | Flat Remix-inspired neutral-white reinterpretation of the same hard disk icon |
