# RISC OS 3.71 desktop resources

These original resources come from the public [`barryc-ro/RiscOS_371`](https://github.com/barryc-ro/RiscOS_371)
tree pinned to `f6c81db7e90f727f3258f874692f751b692a14cb`. Every value below is
the source Git blob SHA-1, which can be retrieved through the GitHub Git Blob
API by that SHA rather than through a mutable branch URL.

| Checked-in resource | Original source path | Git blob SHA-1 |
|---|---|---|
| `sprites/Tools,ff9` | `Sources/OS_Core/Desktop/Wimp/Resources/UK/Tools,ff9` | `298d22d65559f3ffbb73029d3c857331a75f0ce2` |
| `sprites/Tools3d,ff9` | `Sources/OS_Core/Desktop/Wimp/Resources/UK/Tools3d,ff9` | `30149ceb56805c5938108f62c4513528d1c6d52d` |
| `sprites/Sprites22,ff9` | `Sources/OS_Core/Desktop/Wimp/Resources/UK/Sprites22,ff9` | `c64f8aa90a6c2593f55255dd40e29e1d4a009a24` |
| `palettes/8desktop,ffd` | `Sources/OS_Core/Video/Render/Colours/Palettes/8desktop,ffd` | `1dc5e842239e20f3800caa9dc65deda57aac66ea` |
| `fonts/Encodings/.Base0` | `Sources/OS_Core/Video/Render/Fonts/ROMFonts/Fonts/Encodings/.Base0` | `b8aece8a9698b55aa02cde8193b2d1ce1195e24a` |
| `fonts/Encodings/Latin1` | `Sources/OS_Core/Video/Render/Fonts/ROMFonts/Fonts/Encodings/Latin1` | `b016454d018aac54cb158b15c620be465da8c6c6` |
| `fonts/Homerton/Medium/IntMetric0,ff6` | `Sources/OS_Core/Video/Render/Fonts/ROMFonts/Fonts/Homerton/Medium/IntMetric0,ff6` | `0caf1960d6e777b2f1048bf1fd2b3a30c79bd4a6` |
| `fonts/Homerton/Medium/Outlines0,ff6` | `Sources/OS_Core/Video/Render/Fonts/ROMFonts/Fonts/Homerton/Medium/Outlines0,ff6` | `6e2e72b25758a4d5880bb193d78c2df4ecf78a06` |
| `fonts/Corpus/Medium/IntMetric0,ff6` | `Sources/OS_Core/Video/Render/Fonts/ROMFonts/Fonts/Corpus/Medium/IntMetric0,ff6` | `3b8cc63ba8b61c09db88899395a92bd4696350a0` |
| `fonts/Corpus/Medium/Outlines0,ff6` | `Sources/OS_Core/Video/Render/Fonts/ROMFonts/Fonts/Corpus/Medium/Outlines0,ff6` | `fd2d9eb857d8d3b2a230796517be7618bccb8a91` |
| `fonts/Trinity/Medium/IntMetric0,ff6` | `Sources/OS_Core/Video/Render/Fonts/ROMFonts/Fonts/Trinity/Medium/IntMetric0,ff6` | `996cc38852c72ab9e5446519635771fad8c92eab` |
| `fonts/Trinity/Medium/Outlines0,ff6` | `Sources/OS_Core/Video/Render/Fonts/ROMFonts/Fonts/Trinity/Medium/Outlines0,ff6` | `c18d69dc477b6279241cd446fcb0d49dd7724b02` |
| `fonts/System/Medium/IntMetrics,ff6` | `Sources/SystemRes/Fonts/System/Medium/IntMetrics,ff6` | `f13ad9a354474beaf841d508c4cd878e43bec669` |
| `fonts/System/Medium/Outlines,ff6` | `Sources/SystemRes/Fonts/System/Medium/Outlines,ff6` | `fb7b369cef4ec288d6b4b437655d6c05821ab507` |
| `fonts/System/Fixed/IntMetrics,ff6` | `Sources/SystemRes/Fonts/System/Fixed/IntMetrics,ff6` | `43f586351fb64ef2ffd6987c1032ba1b5cad254b` |
| `fonts/System/Fixed/Outlines,ff6` | `Sources/SystemRes/Fonts/System/Fixed/Outlines,ff6` | `4acbd51820e251dce277b465bbe6dfc5112947c9` |
| `fonts/System/Fixed/f240x120,ff6` | `Sources/SystemRes/Fonts/System/Fixed/f240x120,ff6` | `d76149a04ff7e0f47c48953c1d050a4fd39086aa` |
| `fonts/System/Fixed/f240x240,ff6` | `Sources/SystemRes/Fonts/System/Fixed/f240x240,ff6` | `0d14f8ac467e2d8692bd5bea86868757f46ef6e4` |
| `os-source/vdufontl1` | `Sources/OS_Core/Kernel/s/vdu/vdufontl1` | `2faac185375d69d00f9780e155ccaf9c74788143` |

`fonts/System/vdufontl1.bin` is the extracted 224-glyph bitmap for ISO
characters 32–255 from `os-source/vdufontl1`; each glyph is eight rows and bit
7 is the leftmost pixel. The extracted file has Git blob SHA-1
`53a8ac732144f02835829c3d514767b2bcfd14cb` and is golden-tested alongside its
source.

## Reference screenshot texture crops

The raw RGB tiles in `desktop/` were cropped from the RO 3.71 RPCEmu screenshot
provided by the project owner. They preserve the desktop's mottled Acorn
wallpaper, icon-bar stipple, and light window-work-area stipple while excluding
its open window, pointer, and icons. Their formats are tightly packed RGB8
pixels, row-major, without a header: `wallpaper-tile.rgb` is 267×247,
`iconbar-tile.rgb` is 64×68, and `window-tile.rgb` is 22×12. The modern desktop
uses the wallpaper and work-area crops only as faint material texture; the
icon-bar crop remains a visual reference rather than the default appearance.

These screenshot-derived crops are included solely for this project owner's
requested visual comparison. They do not add a reuse or redistribution license
for the original RISC OS artwork.

The pinned upstream snapshot contains no license grant for these resources.
They retain their original authorship and rights; this project does not assign
them the Cargo package's MIT license or grant downstream reuse. Consult the
upstream source and rightsholders before reuse elsewhere.
