# Luna handoff: modern rendering with classic graphics compatibility

## Purpose and status

Implement the rendering direction agreed with the user: a modern RISC OS-inspired desktop using **Vello + wgpu**, with correct classic BASIC graphics and Wimp application contracts. This document is a handoff, not a claim that the architecture or compatibility features are already implemented.

The user has chosen Vello + wgpu based on positive prior experience. Do not reopen the Skia-versus-Vello decision without a concrete blocker. The exact Vello crate/renderer variant and mutually compatible dependency versions still need selection against current upstream releases.

Read `AGENTS.md` and `docs/ricochet-design.md` first. Inspect the live checkout before working: other agents may have changes in progress. Preserve unrelated edits and coordinate ownership before editing shared files. Do not commit unless requested.

## Agreed requirements

- Replace the desktop's fixed pixel-buffer foundation with composition of independent surfaces, paths, text and images at the host display's actual scale.
- Support HiDPI, smooth curves and image sampling, clipping, opacity and restrained shadows.
- Keep classic SWI/VDU contracts and BASIC drawing semantics. Presentation may be asynchronous; observable classic drawing effects may not be.
- In desktop mode, graphics output belongs to the calling task's selected window/content surface. In full-screen mode its selected output occupies the display.
- Never select a drawing destination by keyboard focus or whichever window is frontmost.
- New Filer and system applications use modern drawing while retaining the Wimp window/event/menu/redraw model.
- Existing applications retain compatible drawing, including immediate pixel reads and destination-dependent operations.
- Modern desktop drawing does not require an authoritative CPU pixel copy or synchronous readback.
- Use Inter for the modern desktop; preserve Acorn fonts and their compatibility metrics and encodings.
- Keep desktop policy and application behaviour in BASIC64. Rust supplies rendering, resource management, isolation and other foundational mechanisms.

This is a rendering migration, not permission to replace BASIC64 desktop programs with Rust UI code, change the visual direction, or silently redefine historical SWIs.

## Architectural boundaries

```text
Classic BASIC / compatible Wimp drawing
    -> caller-owned graphics context
    -> authoritative compatibility raster surface
    -> batched texture updates -----------------------+
                                                     |
Modern Wimp applications and desktop components       |
    -> text layout, paths, images, clipped layers ----+
                                                     |
                                     Vello/wgpu compositor
                                                     |
                                    native-scale host display
```

Keep the host window/event integration through winit unless a demonstrated reason requires changing it. Replace pixels as the final desktop rendering abstraction; it may remain temporarily behind a migration switch for comparison and recovery.

The compositor owns placement, stacking, clip regions, transforms, opacity, shadow rendering, resource caches and presentation. It must distinguish **application invalidation** (content needs painting) from **composition damage** (the displayed arrangement changed).

Represent drawing context, surface ownership, resource identity and coordinate conversions explicitly. Guest addresses remain logical and checked; no host/GPU pointers cross the SWI boundary. Task termination releases its surfaces and resources safely.

## Classic raster surfaces and immediate reads

Classic drawing updates authoritative CPU-side state synchronously. A program that plots and immediately reads the same pixel must see the new value before any presentation or GPU upload occurs. The same applies inside a compatible Wimp redraw loop.

Preserve each mode's logical coordinates, pixel grid/aspect ratio, origin, text/graphics clipping, palette, cursor and plot-action semantics. Do not automatically turn classic lines into antialiased vector strokes: that changes coverage, pixel reads, XOR and fill behaviour.

- Batch dirty regions and upload at presentation boundaries; never submit a GPU operation per BASIC PLOT.
- Keep pixel reads and destination-dependent operations off the GPU readback path initially.
- Preserve logical colour indices for indexed modes, or an equivalent representation that correctly supports palette changes and logical operations. Flattening permanently into RGBA is insufficient for those contracts.
- Use RGBA or another suitable explicit representation for true-colour modes.
- Synchronise snapshot/upload access without losing writes or holding a plotting lock through expensive GPU work.
- Allow nearest-neighbour presentation for faithful pixels and filtered presentation when explicitly chosen. Filtering must never change program-visible values.
- Keep raster storage bounded; avoid an ever-growing list of historical plot commands.

Classic content retains its selected resolution when monitor scale changes. Modern furniture around it renders at native resolution. Do not silently reinterpret MODE or increase a guest's pixel dimensions with host DPI.

Some classic operations remain incomplete in the current runtime. Inventory supported and missing operations; preserve existing behaviour and track gaps explicitly. Do not claim complete BBC/RISC OS graphics compatibility merely because a surface exists.

## Output routing and isolation

A graphics context identifies the owning task, destination surface, mode/state, mapping and clip. Multiple windows require explicit context selection. Wimp redraw/update establishes the applicable window context; a non-Wimp BASIC program uses its task-owned output window. Full-screen presentation uses the selected output surface, not a different set of plotting semantics.

Respect explicit sprite/off-screen output redirection. Define context restoration around redraw loops and output switches. Do not redirect unrelated non-graphics SWIs merely because a window is current.

Treat window-scoped output as a documented hosted compatibility policy wherever it differs from historical global screen behaviour. Preserve standard argument blocks, returned values and coordinate conventions; introduce modern capabilities through additive, separately named/versioned extensions rather than repurposing standard fields.

## Wimp redraw protocol

Implement the documented protocol, not an approximation based on repainting a snapshot every frame:

1. Invalid content causes Wimp_Poll to return Redraw_Window_Request.
2. The application calls Wimp_RedrawWindow.
3. It paints the returned region under the corresponding clip.
4. It calls Wimp_GetRectangle repeatedly until no regions remain.

Preserve the contract distinctions:

- Wimp_RedrawWindow and its rectangle loop normally clear returned regions to the window background, subject to the documented transparent-window behaviour.
- Wimp_UpdateWindow begins an application-requested update and preserves existing contents; it also uses Wimp_GetRectangle.
- Wimp_ForceRedraw schedules invalidation for later processing.
- Preserve documented rectangle conventions, ordering constraints, origin/scroll transformations and relevant VDU state. Check the PRM for exact details before implementation.

The caller sees the expected RISC OS coordinates and blocks. Internally, map drawing into window-local storage. Do not expose a changed coordinate system through unchanged SWIs.

Retain backing content so movement and uncovering can reuse valid areas. Request application redraw for missing/stale content, newly required scroll/resize regions and explicit invalidation. Retention is an optimisation: do not invent extra hidden-region drawing requests or relax historical visible-rectangle rules without documenting a deliberate extension. Define how changes made while occluded invalidate cached content so stale pixels are never revealed.

Window movement normally changes composition damage without invalidating content. A desktop shadow change should not invoke application drawing. Publish completed drawing updates coherently; synchronous classic reads remain valid even before presentation.

### Three supported application cases

| Case | Painting lifecycle | Content representation |
| --- | --- | --- |
| Direct classic BASIC | Commands update persistent output | Authoritative compatibility raster |
| Compatible Wimp app | Standard redraw/update rectangle loops | Compatibility raster with synchronous reads |
| Modern Wimp app/system component | Familiar redraw lifecycle and system-managed widgets | Vello drawing content without synchronous CPU readback |

An application being a Wimp task does **not** prove it is safe for GPU-only drawing. Choose the content contract explicitly; legacy behaviour remains the default for existing interfaces.

Initially embed classic content as a separate raster surface inside modern windows. Do not quietly interleave modern GPU drawing with classic pixel reads on the same surface. Such a mixed readable surface needs an explicit rasterisation and ordering contract and is deferred.

## Retention and modern drawing

Choose and document a bounded representation for modern content: retained display items, tiled content, cached render targets, or an appropriate combination. Vello is the renderer; application invalidation and window composition policy are still our responsibility.

Partial redraw must replace/update the affected content without accumulating duplicate drawing commands. Preserve unaffected areas. Wimp_UpdateWindow's preservation semantics must be honoured where exposed. Re-render modern content after display-scale changes rather than simply magnifying a low-resolution cache.

Avoid allocating a texture for the entirety of a potentially enormous scrollable work area. Track viewport/cache bounds and resource limits. Handle GPU/device or surface recreation with recoverable resources and appropriate invalidation.

## Filer, furniture and icons

Filer supplies names, resources, layout and state from BASIC64; common services render labels, selection, icons, menus and furniture. It should not construct folder icons by plotting pixels. Modern system components use semantic styles and modern surfaces while keeping familiar Wimp interaction.

Preserve the accepted desktop decisions:

- Keep the OS Icon at the far right of the iconbar; opened applications may use the space immediately to its left.
- DemoDisk remains on the left. Do not restore inactive placeholder controls.
- Scrollbar width aligns with title-button width.
- Consistent border/stroke treatment; shared edges must not become double thickness.
- Compact RISC OS-inspired menus and layout, with the modern font direction below.

Define stroke widths in logical units and snap appropriate straight edges at the actual device scale. Do not conflate one logical pixel with one physical pixel on HiDPI displays.

Use the existing user-authored OS Icon artwork in `resources/branding/desktop-flat/OSIcon.png`, retaining `OSIcon.svg` as its editable source. Preserve both assets as supplied; do not add a Ricochet wordmark to the icon bar.

Use correct premultiplied-alpha handling, colour-space treatment and high-quality minification/mipmaps as appropriate. Avoid black fringes around transparency. The goal is antialiasing, fractional positioning and quality sampling, not dependence on RGB subpixel smoothing. Verify the visual result at actual icon sizes.

## Typography

Use **Inter** as the bundled, pinned modern desktop family. Include its licence and provenance. Start with Regular for menus/file names/controls, Medium for titles and modest emphasis, and Semibold sparingly. Choose sizes through real desktop specimens rather than inheriting old backing-pixel constants.

Expose semantic roles such as desktop label, window title, body and monospace. New applications request roles; explicit font requests remain possible. Relayout controls using actual measured text, including user text scaling. The modern monospace family and exact fallback bundle are not yet selected.

Retain Homerton, Corpus, Trinity and System resources for compatibility. Never silently alias an explicit Acorn font request to Inter. Preserve original encodings, advances, measurement units, spacing and BASIC cell geometry where required. Sharper rendering must not silently alter layout or observable classic raster results.

Use Parley/Fontique with Vello as the initial modern text-stack candidate. Confirm compatible released versions and functionality. One shaping/layout result must drive measurement, painting, caret placement, selection and hit testing. Include Unicode shaping, bidi and script-aware fallback; Inter alone is not universal coverage. Keep fallback deterministic across hosts where practical and document the bundled coverage versus host fallback.

Font profile is distinct from rendering backend. Do not force every legacy application to use modern layout just because Vello presents its window. Conversely, preserve the ability to explicitly use an Acorn font in modern content without pretending that modern Unicode shaping is identical to the original Font Manager contract.

## Current source map: verify before editing

- `src/graphics.rs`: GraphicsService, snapshots, VDU parser and shared raster storage.
- `src/renderer.rs`: current compatibility rendering and CPU desktop composition.
- `src/window.rs`: winit/pixels integration, display lifecycle and event handling.
- `src/wimp.rs`: task/window/icon/menu state, routing and SWI mechanisms.
- `src/swi.rs`, `src/runtime.rs`: caller/service integration; inspect actual ownership before modifying.
- `src/riscos_font.rs`, `src/riscos_resources.rs`: existing font/resource implementation.
- `demo-volume/System/Desktop.bas64`, `demo-volume/System/Filer.bas64`: desktop policy and Filer.
- `src/snapshot.rs`, `tests/wimp_desktop.rs`: existing visual/integration checks.

These are starting points, not a guarantee of current implementation status. In particular, inspect redraw SWI support and existing tests rather than assuming the design brief describes completed functionality.

## Implementation sequence

### 1. Audit and establish contracts

Inventory supported graphics/SWIs, surface ownership, redraw gaps and current tests. Record dependency choices and baseline screenshots. Define surface types, context selection, scale/coordinate mapping and invalidation interfaces. Update the design brief to reflect the agreed direction and distinguish shipped work from planned work.

### 2. Vello/wgpu presentation slice

Create the modern presentation path with a native-scale host surface, Inter text, clipped overlapping content and a restrained shadow. Compose an existing classic raster texture in the same frame. Validate alpha, scaling and device lifecycle before migrating all furniture. Retain a comparison/recovery path temporarily.

### 3. Authoritative compatibility surfaces

Establish task/surface isolation, synchronous reads, palette semantics and dirty uploads. Route non-Wimp output correctly in desktop and full-screen modes. Keep plot-heavy workloads batched and bounded.

### 4. Wimp redraw/update machinery

Implement the actual rectangle protocol with ownership checks and standard blocks, backed by retained window content. Test invalidation, scrolling, resizing, occlusion and immediate reads inside redraws. Do not substitute forced full-window redraw on every frame as the final implementation.

### 5. Modern system UI and fonts

Migrate furniture, menus, icons and Filer to modern drawing and semantic font roles. Add/document minimal additive interfaces needed by BASIC64. Keep the classic content path available inside modern windows.

### 6. Validate, document and retire transitional dependencies

Verify behaviour and visuals, measure representative workloads and address regressions. Remove pixels from the main presentation path once coverage supports it. Record remaining compatibility gaps and deferred work honestly.

Deliver runnable vertical slices. Do not mark the overhaul complete after a static Vello scene or a dependency swap.

## Acceptance checks

### Compatibility and isolation

- Plot then read immediately, without waiting for a frame, returns the correct value.
- Exercise supported overwrite/logical actions, clipping, origins, palette changes and text/graphics state. Add targeted cases as missing semantics are implemented.
- Two tasks cannot draw into each other's windows; focus changes do not reroute output.
- Multiple windows and explicit output redirection restore contexts correctly.
- Full-screen and windowed presentation preserve identical guest-visible drawing results.
- Classic text metrics/cell geometry remain stable; explicit Acorn font selection remains available.

### Wimp lifecycle

- Redraw requests and rectangle iteration follow the documented contract, including completion and clearing.
- UpdateWindow preserves prior content; ForceRedraw schedules repaint.
- Returned coordinates/clips work with scrolling and existing application calculations.
- Moving/uncovering valid content does not require unnecessary app repaint; invalid hidden content is not revealed stale.
- Resize/scroll exposes correct new content; partial redraw does not erase unrelated areas or accumulate stale scene commands.

### Modern visuals and text

- Inspect 1× and 2× plus fractional scale where supported. Input mapping matches visuals at each scale.
- Inter labels, menu alignment, selections, clipping and title/scrollbar alignment remain correct.
- The user-authored OS Icon appears at the far right of the icon bar; no project wordmark appears in the desktop shell.
- Shadows do not affect guest pixel reads or application hit regions.
- Text measurement and painting agree; exercise fallback and bidi, not just ASCII.

### Performance and robustness

- A plot-heavy workload batches uploads rather than issuing one submission per plot.
- Window movement does not rerun unchanged BASIC drawing or text layout every frame.
- Resource/scene memory stays bounded under repeated redraw, scrolling and task launch/exit.
- Surface resize, scale changes and GPU resource recreation recover cleanly.
- Report measured timings/workloads rather than promising a frame rate without evidence.

Run focused tests for changed mechanisms, existing relevant regression suites and visual snapshots. Compare guest-visible values separately from presentation screenshots. Report commands, results, baseline failures and unverified areas. Preserve unrelated working-tree edits.

## Deferred or unresolved choices

- Exact Vello renderer variant and dependency versions; prove the required image/effect/font integration with the chosen releases.
- Exact modern drawing extension API and versioning, without changing standard Wimp blocks.
- Modern monospace family, fallback coverage and final UI sizes.
- Cache/tile strategy, presentation granularity and resource budgets.
- Readback semantics for a surface mixing modern GPU content and classic operations. Keep separate initially.
- Broader missing historical graphics/SWI behaviour beyond the validated compatibility scope.

## References

- Project direction: `docs/ricochet-design.md`.
- RISC OS Wimp contracts: https://www.riscos.com/support/developers/prm/wimp.html
- Vello renderer choices: https://github.com/linebender/vello
- wgpu: https://docs.rs/wgpu/latest/wgpu/
- Parley and Fontique: https://github.com/linebender/parley
- Inter: https://rsms.me/inter/

Consult the authoritative documentation for precise public contracts and current dependency APIs. Keep firm user decisions above distinct from implementation suggestions and unresolved choices.
