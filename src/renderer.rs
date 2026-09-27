//! Rasterizes the hosted BBC screen scene into the shared pixel framebuffer.

use crate::{
    font::bbc_micro_glyph,
    graphics::{GraphicsPrimitive, GraphicsSnapshot, GraphicsWindow, Point, graphics_colour},
    riscos_resources::{RiscOsSprite, RiscOsSpriteFile, SpriteSet, system_bitmap_font},
    wimp::{
        DESKTOP_HEIGHT, DESKTOP_OS_UNITS_PER_PIXEL_X, DESKTOP_OS_UNITS_PER_PIXEL_Y,
        DESKTOP_PIXEL_HEIGHT, DESKTOP_PIXEL_WIDTH, DesktopRect, DesktopWindow,
        VerticalScrollbarLayout, WindowFurnitureLayout, WorkArea,
    },
};
use std::time::{SystemTime, UNIX_EPOCH};
use std::{collections::HashMap, sync::OnceLock};

pub const SCREEN_WIDTH: u32 = 640;
pub const SCREEN_HEIGHT: u32 = 256;
const BYTES_PER_PIXEL: usize = 4;

/// Paint graphics and the BBC bitmap text into one RGBA frame.
pub fn render(snapshot: &GraphicsSnapshot, frame: &mut [u8]) {
    let width = snapshot.mode.pixel_width;
    let height = snapshot.mode.pixel_height;
    let expected_size = width as usize * height as usize * BYTES_PER_PIXEL;
    if frame.len() < expected_size {
        return;
    }

    if let Some(surface) = &snapshot.raster_surface {
        surface.copy_to(&mut frame[..expected_size]);
    } else {
        for pixel in frame[..expected_size].chunks_exact_mut(BYTES_PER_PIXEL) {
            pixel.copy_from_slice(&[0, 0, 0, 0xFF]);
        }

        for primitive in &snapshot.primitives {
            match primitive {
                GraphicsPrimitive::Line {
                    from,
                    to,
                    logical_colour,
                    clip,
                    ..
                } => {
                    if let Some((from, to)) = clip_line(*from, *to, *clip, snapshot) {
                        let from = screen_point(from, snapshot);
                        let to = screen_point(to, snapshot);
                        draw_line(
                            frame,
                            width,
                            height,
                            from,
                            to,
                            colour(*logical_colour, snapshot),
                        );
                    }
                }
                GraphicsPrimitive::Point {
                    at,
                    logical_colour,
                    clip,
                    ..
                } => {
                    if inside(*at, *clip, snapshot) {
                        let (x, y) = screen_point(*at, snapshot);
                        set_pixel(
                            frame,
                            width,
                            height,
                            x,
                            y,
                            colour(*logical_colour, snapshot),
                        );
                    }
                }
            }
        }
    }

    draw_text(snapshot, frame, width, height);
    draw_cursor(snapshot, frame, width, height);
}

/// Render an application snapshot in the desktop text profile. Graphics and
/// pixel colours are unchanged; only the glyph source differs from the BBC
/// compatibility framebuffer above.
pub fn render_desktop_content(snapshot: &GraphicsSnapshot, frame: &mut [u8]) {
    let width = snapshot.mode.pixel_width;
    let height = snapshot.mode.pixel_height;
    let expected_size = width as usize * height as usize * BYTES_PER_PIXEL;
    if frame.len() < expected_size {
        return;
    }

    if let Some(surface) = &snapshot.raster_surface {
        surface.copy_to(&mut frame[..expected_size]);
    } else {
        for pixel in frame[..expected_size].chunks_exact_mut(BYTES_PER_PIXEL) {
            pixel.copy_from_slice(&[0, 0, 0, 0xFF]);
        }
        for primitive in &snapshot.primitives {
            match primitive {
                GraphicsPrimitive::Line {
                    from,
                    to,
                    logical_colour,
                    clip,
                    ..
                } => {
                    if let Some((from, to)) = clip_line(*from, *to, *clip, snapshot) {
                        draw_line(
                            frame,
                            width,
                            height,
                            screen_point(from, snapshot),
                            screen_point(to, snapshot),
                            colour(*logical_colour, snapshot),
                        );
                    }
                }
                GraphicsPrimitive::Point {
                    at,
                    logical_colour,
                    clip,
                    ..
                } => {
                    if inside(*at, *clip, snapshot) {
                        let (x, y) = screen_point(*at, snapshot);
                        set_pixel(
                            frame,
                            width,
                            height,
                            x,
                            y,
                            colour(*logical_colour, snapshot),
                        );
                    }
                }
            }
        }
    }

    draw_desktop_text(snapshot, frame, width, height);
}

/// Render the hosted two-task Wimp desktop. Application contents use the
/// first-slice task-output adapter documented in the design brief; the window
/// manager still owns all frame, stacking and input routing.
pub fn render_desktop(
    windows: &[DesktopWindow],
    scenes: &HashMap<u64, GraphicsSnapshot>,
    frame: &mut [u8],
) {
    let width = DESKTOP_PIXEL_WIDTH;
    let height = DESKTOP_PIXEL_HEIGHT;
    let bytes = width as usize * height as usize * BYTES_PER_PIXEL;
    if frame.len() < bytes {
        return;
    }
    for (index, pixel) in frame[..bytes].chunks_exact_mut(BYTES_PER_PIXEL).enumerate() {
        // RISC OS 3's neutral desktop grey, with a restrained two-tone
        // texture instead of a modern saturated wallpaper.
        let shade = if (index % width as usize + index / width as usize) % 8 == 0 {
            194
        } else {
            190
        };
        pixel.copy_from_slice(&[shade, shade, shade, 255]);
    }

    // `windows` is front-to-back; paint from the back towards the front.
    for window in windows.iter().rev() {
        draw_guest_window(window, scenes.get(&window.owner_task_id), frame);
    }
}

fn draw_guest_window(window: &DesktopWindow, scene: Option<&GraphicsSnapshot>, frame: &mut [u8]) {
    let width = DESKTOP_PIXEL_WIDTH;
    let height = DESKTOP_PIXEL_HEIGHT;
    let sprites = desktop_sprites();
    let current = WindowFurnitureLayout::new(
        window.work_area,
        window.work_extent,
        window.scroll_x,
        window.scroll_y,
        window.has_back_icon,
        window.has_title,
        window.closable,
        window.has_toggle_size_icon,
        window.has_vertical_scrollbar,
        window.resizable,
    );
    let outer = to_pixel_rect(current.outer);
    let work = to_pixel_rect(current.work_area);
    if outer.is_empty() || work.is_empty() {
        return;
    }

    fill_rect(
        frame,
        width,
        height,
        outer.left,
        outer.top,
        outer.right,
        outer.bottom,
        [150, 150, 150, 255],
    );
    draw_bevel(frame, outer);
    fill_rect(
        frame,
        width,
        height,
        work.left,
        work.top,
        work.right,
        work.bottom,
        [211, 211, 211, 255],
    );
    draw_work_texture(frame, work);

    if let Some(snapshot) = scene {
        draw_task_scene(
            snapshot,
            frame,
            work,
            window.work_extent,
            window
                .preview_scroll
                .unwrap_or((window.scroll_x, window.scroll_y)),
        );
    }

    if window.has_title {
        draw_title_bar(frame, current, window, sprites);
    }
    if let Some(scrollbar) = current.vertical_scrollbar {
        draw_vertical_scrollbar(frame, scrollbar, sprites);
    }
    if let Some(icon) = current.adjust_size_icon {
        draw_sprite_in_cell(frame, icon, sprites.get("sicon22"));
    }

    if let Some(preview) = window.preview_area {
        let preview_layout = WindowFurnitureLayout::new(
            preview,
            window.work_extent,
            window
                .preview_scroll
                .unwrap_or((window.scroll_x, window.scroll_y))
                .0,
            window
                .preview_scroll
                .unwrap_or((window.scroll_x, window.scroll_y))
                .1,
            window.has_back_icon,
            window.has_title,
            window.closable,
            window.has_toggle_size_icon,
            window.has_vertical_scrollbar,
            window.resizable,
        );
        draw_preview_outline(frame, preview_layout.outer);
    }
}

fn draw_task_scene(
    snapshot: &GraphicsSnapshot,
    frame: &mut [u8],
    clip: PixelRect,
    extent: WorkArea,
    scroll: (i32, i32),
) {
    let source_width = snapshot.mode.pixel_width;
    let source_height = snapshot.mode.pixel_height;
    if source_width == 0 || source_height == 0 || clip.is_empty() {
        return;
    }
    let mut source = vec![0; source_width as usize * source_height as usize * BYTES_PER_PIXEL];
    render_desktop_content(snapshot, &mut source);

    // The task display is a fixed-resolution application surface. Its mode
    // contributes its own OS-unit pixel ratios; the desktop's mode-20 pixels
    // are converted separately. The work-area size never stretches text.
    let source_osu_x = mode_osu_per_pixel(snapshot.mode.logical_width, source_width);
    let source_osu_y = mode_osu_per_pixel(snapshot.mode.logical_height, source_height);
    for target_y in clip.top.max(0)..clip.bottom.min(DESKTOP_PIXEL_HEIGHT as i32) {
        let work_y = extent.max_y - scroll.1 + (target_y - clip.top) * DESKTOP_OS_UNITS_PER_PIXEL_Y;
        let sy = work_y.div_euclid(source_osu_y);
        if sy < 0 || sy >= source_height as i32 {
            continue;
        }
        for target_x in clip.left.max(0)..clip.right.min(DESKTOP_PIXEL_WIDTH as i32) {
            let work_x =
                scroll.0 - extent.min_x + (target_x - clip.left) * DESKTOP_OS_UNITS_PER_PIXEL_X;
            let sx = work_x.div_euclid(source_osu_x);
            if sx < 0 || sx >= source_width as i32 {
                continue;
            }
            let source_at = (sy as usize * source_width as usize + sx as usize) * BYTES_PER_PIXEL;
            let target_at = (target_y as usize * DESKTOP_PIXEL_WIDTH as usize + target_x as usize)
                * BYTES_PER_PIXEL;
            frame[target_at..target_at + BYTES_PER_PIXEL]
                .copy_from_slice(&source[source_at..source_at + BYTES_PER_PIXEL]);
        }
    }
}

fn mode_osu_per_pixel(logical_extent: i32, pixel_extent: u32) -> i32 {
    if logical_extent <= 0 || pixel_extent == 0 {
        return DESKTOP_OS_UNITS_PER_PIXEL_X;
    }
    (logical_extent / pixel_extent as i32).max(1)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct PixelRect {
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
}

impl PixelRect {
    fn is_empty(self) -> bool {
        self.right <= self.left || self.bottom <= self.top
    }
}

fn to_pixel_rect(rect: DesktopRect) -> PixelRect {
    let scale_x = DESKTOP_OS_UNITS_PER_PIXEL_X;
    let scale_y = DESKTOP_OS_UNITS_PER_PIXEL_Y;
    PixelRect {
        left: rect.min_x.div_euclid(scale_x),
        right: div_ceil(rect.max_x, scale_x),
        top: (DESKTOP_HEIGHT - rect.max_y).div_euclid(scale_y),
        bottom: div_ceil(DESKTOP_HEIGHT - rect.min_y, scale_y),
    }
}

fn div_ceil(value: i32, divisor: i32) -> i32 {
    value.div_euclid(divisor) + i32::from(value.rem_euclid(divisor) != 0)
}

fn desktop_sprites() -> &'static RiscOsSpriteFile {
    static SPRITES: OnceLock<RiscOsSpriteFile> = OnceLock::new();
    SPRITES.get_or_init(|| {
        RiscOsSpriteFile::builtin(SpriteSet::Tools3d)
            .expect("pinned RISC OS 3.71 Wimp sprites parse and verify")
    })
}

fn draw_bevel(frame: &mut [u8], rect: PixelRect) {
    let width = DESKTOP_PIXEL_WIDTH;
    let height = DESKTOP_PIXEL_HEIGHT;
    draw_line(
        frame,
        width,
        height,
        (rect.left, rect.top),
        (rect.right - 1, rect.top),
        [250, 250, 250, 255],
    );
    draw_line(
        frame,
        width,
        height,
        (rect.left, rect.top),
        (rect.left, rect.bottom - 1),
        [250, 250, 250, 255],
    );
    draw_line(
        frame,
        width,
        height,
        (rect.left, rect.bottom - 1),
        (rect.right - 1, rect.bottom - 1),
        [82, 82, 82, 255],
    );
    draw_line(
        frame,
        width,
        height,
        (rect.right - 1, rect.top),
        (rect.right - 1, rect.bottom - 1),
        [82, 82, 82, 255],
    );
}

fn draw_work_texture(frame: &mut [u8], rect: PixelRect) {
    for y in rect.top.max(0)..rect.bottom.min(DESKTOP_PIXEL_HEIGHT as i32) {
        for x in rect.left.max(0)..rect.right.min(DESKTOP_PIXEL_WIDTH as i32) {
            if (x + y) % 4 == 0 {
                set_pixel(
                    frame,
                    DESKTOP_PIXEL_WIDTH,
                    DESKTOP_PIXEL_HEIGHT,
                    x,
                    y,
                    [207, 207, 207, 255],
                );
            }
        }
    }
}

fn draw_title_bar(
    frame: &mut [u8],
    layout: WindowFurnitureLayout,
    window: &DesktopWindow,
    sprites: &RiscOsSpriteFile,
) {
    let header = DesktopRect {
        min_x: layout.outer.min_x,
        min_y: layout.work_area.max_y,
        max_x: layout.outer.max_x,
        max_y: layout.outer.max_y - 2,
    };
    let band = to_pixel_rect(header);
    if band.is_empty() {
        return;
    }
    let inner_top = band.top + 1;
    let inner_bottom = band.bottom - 1;
    let inner_left = band.left + 1;
    let inner_right = band.right - 1;
    let middle_width = inner_right - inner_left - 4;
    let top_height = ((inner_bottom - inner_top) / 2).max(1);
    let bottom_height = inner_bottom - inner_top - top_height;
    // The native title sprites use transparent pixels for their light-grey
    // weave. Seed the strip with the Wimp's light-grey neutral so transparency
    // does not expose the dark outer window frame below it.
    fill_rect(
        frame,
        DESKTOP_PIXEL_WIDTH,
        DESKTOP_PIXEL_HEIGHT,
        inner_left,
        inner_top,
        inner_right,
        inner_bottom,
        [221, 221, 221, 255],
    );
    tile_sprite_region(
        frame,
        sprites.get("tbarmidt22"),
        PixelRect {
            left: inner_left + 2,
            top: inner_top,
            right: inner_left + 2 + middle_width,
            bottom: inner_top + top_height,
        },
    );
    tile_sprite_region(
        frame,
        sprites.get("tbarmidb22"),
        PixelRect {
            left: inner_left + 2,
            top: inner_top + top_height,
            right: inner_left + 2 + middle_width,
            bottom: inner_top + top_height + bottom_height,
        },
    );
    draw_sprite_at(frame, sprites.get("tbarlcap22"), inner_left, inner_top);
    draw_sprite_at(frame, sprites.get("tbarrcap22"), inner_right - 2, inner_top);

    if let Some(icon) = layout.back_icon {
        draw_sprite_in_cell(frame, icon, sprites.get("bicon22"));
    }
    if let Some(icon) = layout.close_icon {
        draw_sprite_in_cell(frame, icon, sprites.get("cicon22"));
    }
    if let Some(icon) = layout.toggle_size_icon {
        let name = if window.maximized {
            "ticon122"
        } else {
            "ticon22"
        };
        draw_sprite_in_cell(frame, icon, sprites.get(name));
    }
    if !window.title.is_empty() {
        if let Some(title) = layout.title_bar {
            let rect = to_pixel_rect(title);
            let text_x = rect.left + 3;
            let text_y = band.top + ((band.bottom - band.top - 16) / 2).max(0);
            draw_system_bitmap_text(frame, text_x, text_y, &window.title, rect, window.focused);
        }
    }
}

fn draw_system_bitmap_text(
    frame: &mut [u8],
    x: i32,
    y: i32,
    text: &str,
    clip: PixelRect,
    focused: bool,
) {
    let font = system_bitmap_font();
    let color = if focused {
        [32, 32, 32, 255]
    } else {
        [72, 72, 72, 255]
    };
    let mut cursor_x = x;
    for character in text.chars() {
        let Ok(codepoint) = u8::try_from(u32::from(character)) else {
            continue;
        };
        let Some(glyph) = font.glyph(codepoint) else {
            cursor_x += 8;
            continue;
        };
        for (row, bits) in glyph.iter().enumerate() {
            for column in 0..8 {
                if bits & (0x80 >> column) == 0 {
                    continue;
                }
                for dy in 0..2 {
                    let px = cursor_x + column as i32;
                    let py = y + row as i32 * 2 + dy;
                    if px >= clip.left && px < clip.right && py >= clip.top && py < clip.bottom {
                        set_pixel(
                            frame,
                            DESKTOP_PIXEL_WIDTH,
                            DESKTOP_PIXEL_HEIGHT,
                            px,
                            py,
                            color,
                        );
                    }
                }
            }
        }
        cursor_x += (font.advance_osu(codepoint) / DESKTOP_OS_UNITS_PER_PIXEL_X).max(1);
    }
}

fn draw_vertical_scrollbar(
    frame: &mut [u8],
    layout: VerticalScrollbarLayout,
    sprites: &RiscOsSpriteFile,
) {
    let bounds = to_pixel_rect(layout.bounds);
    let well_left = bounds.left + ((bounds.right - bounds.left - 20) / 2).max(0);
    tile_sprite_region(
        frame,
        sprites.get("vwellt22"),
        PixelRect {
            left: well_left,
            top: bounds.top,
            right: well_left + 20,
            bottom: bounds.bottom,
        },
    );
    if let Some(sprite) = sprites.get("vwelltcap22") {
        draw_sprite_at(
            frame,
            Some(sprite),
            bounds.left + (bounds.right - bounds.left - sprite.width as i32) / 2,
            bounds.top,
        );
    }
    if let Some(sprite) = sprites.get("vwellbcap22") {
        draw_sprite_at(
            frame,
            Some(sprite),
            bounds.left + (bounds.right - bounds.left - sprite.width as i32) / 2,
            bounds.bottom - sprite.height as i32,
        );
    }
    draw_sprite_in_cell(frame, layout.up_arrow, sprites.get("uicon22"));
    draw_sprite_in_cell(frame, layout.down_arrow, sprites.get("dicon22"));

    let slider = to_pixel_rect(layout.slider);
    let x = slider.left + ((slider.right - slider.left - 20) / 2).max(0);
    let mut y = slider.top;
    if let Some(sprite) = sprites.get("vbart22") {
        draw_sprite_at(frame, Some(sprite), x, y);
        y += sprite.height as i32;
    }
    if let Some(sprite) = sprites.get("vbarb22") {
        let bottom_y = slider.bottom - sprite.height as i32;
        if let Some(mid) = sprites.get("vbarmid22") {
            tile_sprite_region(
                frame,
                Some(mid),
                PixelRect {
                    left: x,
                    top: y,
                    right: x + 20,
                    bottom: bottom_y,
                },
            );
        }
        draw_sprite_at(frame, Some(sprite), x, bottom_y);
    }
}

fn draw_sprite_in_cell(frame: &mut [u8], cell: DesktopRect, sprite: Option<&RiscOsSprite>) {
    let Some(sprite) = sprite else {
        return;
    };
    let rect = to_pixel_rect(cell);
    let x = rect.left + ((rect.right - rect.left - sprite.width as i32) / 2).max(0);
    let y = rect.top + ((rect.bottom - rect.top - sprite.height as i32) / 2).max(0);
    draw_sprite_at(frame, Some(sprite), x, y);
}

fn draw_sprite_at(frame: &mut [u8], sprite: Option<&RiscOsSprite>, x: i32, y: i32) {
    let Some(sprite) = sprite else {
        return;
    };
    for row in 0..sprite.height as i32 {
        for column in 0..sprite.width as i32 {
            let Some(color) = sprite.pixel(column as u32, row as u32) else {
                continue;
            };
            if color[3] != 0 {
                set_pixel(
                    frame,
                    DESKTOP_PIXEL_WIDTH,
                    DESKTOP_PIXEL_HEIGHT,
                    x + column,
                    y + row,
                    color,
                );
            }
        }
    }
}

fn tile_sprite_region(frame: &mut [u8], sprite: Option<&RiscOsSprite>, region: PixelRect) {
    let Some(sprite) = sprite else {
        return;
    };
    if region.is_empty() || sprite.width == 0 || sprite.height == 0 {
        return;
    }
    for y in region.top.max(0)..region.bottom.min(DESKTOP_PIXEL_HEIGHT as i32) {
        for x in region.left.max(0)..region.right.min(DESKTOP_PIXEL_WIDTH as i32) {
            let sx = (x - region.left).rem_euclid(sprite.width as i32) as u32;
            let sy = (y - region.top).rem_euclid(sprite.height as i32) as u32;
            if let Some(color) = sprite.pixel(sx, sy).filter(|pixel| pixel[3] != 0) {
                set_pixel(
                    frame,
                    DESKTOP_PIXEL_WIDTH,
                    DESKTOP_PIXEL_HEIGHT,
                    x,
                    y,
                    color,
                );
            }
        }
    }
}

fn draw_preview_outline(frame: &mut [u8], outer: DesktopRect) {
    let rect = to_pixel_rect(outer);
    draw_line(
        frame,
        DESKTOP_PIXEL_WIDTH,
        DESKTOP_PIXEL_HEIGHT,
        (rect.left, rect.top),
        (rect.right - 1, rect.top),
        [66, 66, 66, 255],
    );
    draw_line(
        frame,
        DESKTOP_PIXEL_WIDTH,
        DESKTOP_PIXEL_HEIGHT,
        (rect.right - 1, rect.top),
        (rect.right - 1, rect.bottom - 1),
        [66, 66, 66, 255],
    );
    draw_line(
        frame,
        DESKTOP_PIXEL_WIDTH,
        DESKTOP_PIXEL_HEIGHT,
        (rect.right - 1, rect.bottom - 1),
        (rect.left, rect.bottom - 1),
        [66, 66, 66, 255],
    );
    draw_line(
        frame,
        DESKTOP_PIXEL_WIDTH,
        DESKTOP_PIXEL_HEIGHT,
        (rect.left, rect.bottom - 1),
        (rect.left, rect.top),
        [66, 66, 66, 255],
    );
}

fn fill_rect(
    frame: &mut [u8],
    width: u32,
    height: u32,
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
    color: [u8; 4],
) {
    for y in top.max(0)..bottom.min(height as i32) {
        for x in left.max(0)..right.min(width as i32) {
            set_pixel(frame, width, height, x, y, color);
        }
    }
}

fn draw_text(snapshot: &GraphicsSnapshot, frame: &mut [u8], width: u32, height: u32) {
    if is_teletext_mode(snapshot.mode.number) {
        draw_teletext(snapshot, frame, width, height);
        return;
    }
    let columns = u32::from(snapshot.mode.text_columns);
    let rows = u32::from(snapshot.mode.text_rows);
    if columns == 0 || rows == 0 {
        return;
    }

    let text_colour = colour(u32::from(snapshot.text_colour), snapshot);

    for (index, character) in snapshot.text_cells.iter().copied().enumerate() {
        if character == b' ' {
            continue;
        }
        let Some(glyph) = bbc_micro_glyph(character) else {
            continue;
        };
        let column = index as u32 % columns;
        let row = index as u32 / columns;
        if row >= rows {
            break;
        }

        let left = column * width / columns;
        let right = (column + 1) * width / columns;
        let top = row * height / rows;
        let bottom = (row + 1) * height / rows;
        let cell_width = right - left;
        let cell_height = bottom - top;
        for y in 0..cell_height {
            let glyph_y = (y * 8 / cell_height) as usize;
            let bits = glyph[glyph_y];
            for x in 0..cell_width {
                let glyph_x = (x * 8 / cell_width) as u8;
                if bits & (0x80 >> glyph_x) != 0 {
                    set_pixel(
                        frame,
                        width,
                        height,
                        (left + x) as i32,
                        (top + y) as i32,
                        text_colour,
                    );
                }
            }
        }
    }
}

fn draw_desktop_text(snapshot: &GraphicsSnapshot, frame: &mut [u8], width: u32, height: u32) {
    let columns = u32::from(snapshot.mode.text_columns);
    let rows = u32::from(snapshot.mode.text_rows);
    if columns == 0 || rows == 0 {
        return;
    }
    let font = system_bitmap_font();
    let text_color = colour(u32::from(snapshot.text_colour), snapshot);
    for (index, character) in snapshot.text_cells.iter().copied().enumerate() {
        if character == b' ' {
            continue;
        }
        let Some(glyph) = font.glyph(character) else {
            continue;
        };
        let column = index as u32 % columns;
        let row = index as u32 / columns;
        if row >= rows {
            break;
        }
        let left = column * width / columns;
        let right = (column + 1) * width / columns;
        let top = row * height / rows;
        let bottom = (row + 1) * height / rows;
        let cell_width = right - left;
        let cell_height = bottom - top;
        if cell_width == 0 || cell_height == 0 {
            continue;
        }
        for y in 0..cell_height {
            let glyph_y = (y * 8 / cell_height) as usize;
            let bits = glyph[glyph_y];
            for x in 0..cell_width {
                let glyph_x = (x * 8 / cell_width) as u8;
                if bits & (0x80 >> glyph_x) != 0 {
                    set_pixel(
                        frame,
                        width,
                        height,
                        (left + x) as i32,
                        (top + y) as i32,
                        text_color,
                    );
                }
            }
        }
    }
}

#[derive(Clone, Copy)]
struct TeletextAttributes {
    foreground: u8,
    background: u8,
    graphics: bool,
    separated: bool,
    flashing: bool,
    double_height: bool,
    conceal: bool,
    hold_graphics: bool,
}

impl Default for TeletextAttributes {
    fn default() -> Self {
        Self {
            foreground: 7,
            background: 0,
            graphics: false,
            separated: false,
            flashing: false,
            double_height: false,
            conceal: false,
            hold_graphics: false,
        }
    }
}

fn is_teletext_mode(number: u8) -> bool {
    number == 7 || number == 135
}

fn draw_teletext(snapshot: &GraphicsSnapshot, frame: &mut [u8], width: u32, height: u32) {
    let columns = u32::from(snapshot.mode.text_columns);
    let rows = u32::from(snapshot.mode.text_rows);
    if columns == 0 || rows == 0 {
        return;
    }
    let flash_visible = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|time| (time.as_millis() / 500) % 2 == 0)
        .unwrap_or(true);

    for row in 0..rows {
        let mut attributes = TeletextAttributes::default();
        let mut held_graphic = None;
        for column in 0..columns {
            let index = (row * columns + column) as usize;
            let character = snapshot.text_cells.get(index).copied().unwrap_or(b' ');
            let control = (0x80..=0x9F).contains(&character);
            let displayed_character = if control {
                if attributes.hold_graphics && attributes.graphics {
                    held_graphic
                } else {
                    None
                }
            } else if attributes.graphics && teletext_mosaic_pattern(character).is_some() {
                held_graphic = Some(character);
                Some(character)
            } else {
                Some(character)
            };

            draw_teletext_cell(
                snapshot,
                frame,
                width,
                height,
                column,
                row,
                displayed_character,
                attributes,
                flash_visible,
            );

            if control {
                apply_teletext_control(&mut attributes, character);
            }
        }
    }
}

fn apply_teletext_control(attributes: &mut TeletextAttributes, control: u8) {
    match control {
        0x80 | 0x90 => {
            attributes.foreground = 0;
            attributes.graphics = false;
            attributes.conceal = false;
        }
        0x81..=0x87 => {
            attributes.foreground = control - 0x80;
            attributes.graphics = false;
            attributes.conceal = false;
        }
        0x88 => attributes.flashing = true,
        0x89 => attributes.flashing = false,
        0x8C => attributes.double_height = false,
        0x8D => attributes.double_height = true,
        0x91..=0x97 => {
            attributes.foreground = control - 0x90;
            attributes.graphics = true;
            attributes.conceal = false;
        }
        0x98 => attributes.conceal = true,
        0x99 => attributes.separated = false,
        0x9A => attributes.separated = true,
        0x9C => attributes.background = 0,
        0x9D => attributes.background = attributes.foreground,
        0x9E => attributes.hold_graphics = true,
        0x9F => attributes.hold_graphics = false,
        _ => {}
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_teletext_cell(
    snapshot: &GraphicsSnapshot,
    frame: &mut [u8],
    width: u32,
    height: u32,
    column: u32,
    row: u32,
    character: Option<u8>,
    attributes: TeletextAttributes,
    flash_visible: bool,
) {
    let columns = u32::from(snapshot.mode.text_columns);
    let rows = u32::from(snapshot.mode.text_rows);
    let left = column * width / columns;
    let right = (column + 1) * width / columns;
    let top = row * height / rows;
    let bottom = (row + 1) * height / rows;
    let cell_width = right - left;
    let cell_height = bottom - top;
    let background = graphics_colour(u32::from(attributes.background), snapshot.mode);
    for y in top..bottom {
        for x in left..right {
            set_pixel(frame, width, height, x as i32, y as i32, background);
        }
    }

    if attributes.conceal || (attributes.flashing && !flash_visible) {
        return;
    }
    let Some(character) = character else {
        return;
    };
    let foreground = graphics_colour(u32::from(attributes.foreground), snapshot.mode);

    if attributes.graphics {
        if let Some(pattern) = teletext_mosaic_pattern(character) {
            for sixel_y in 0..3_u32 {
                for sixel_x in 0..2_u32 {
                    let bit = 4 - sixel_y * 2 + sixel_x;
                    if pattern & (1 << bit) == 0 {
                        continue;
                    }
                    let mut x0 = left + sixel_x * cell_width / 2;
                    let mut x1 = left + (sixel_x + 1) * cell_width / 2;
                    let mut y0 = top + sixel_y * cell_height / 3;
                    let mut y1 = top + (sixel_y + 1) * cell_height / 3;
                    if attributes.separated {
                        x0 += 1;
                        x1 = x1.saturating_sub(1);
                        y0 += 1;
                        y1 = y1.saturating_sub(1);
                    }
                    for y in y0..y1 {
                        for x in x0..x1 {
                            set_pixel(frame, width, height, x as i32, y as i32, foreground);
                        }
                    }
                }
            }
            return;
        }
    }

    let character = character & 0x7F;
    let Some(glyph) = bbc_micro_glyph(character) else {
        return;
    };
    for y in 0..cell_height {
        let glyph_y = if attributes.double_height {
            let half = if row % 2 == 0 { 0 } else { 4 };
            half + (y * 4 / cell_height) as usize
        } else {
            (y * 8 / cell_height) as usize
        };
        let bits = glyph[glyph_y.min(7)];
        for x in 0..cell_width {
            let glyph_x = (x * 8 / cell_width) as u8;
            if bits & (0x80 >> glyph_x) != 0 {
                set_pixel(
                    frame,
                    width,
                    height,
                    (left + x) as i32,
                    (top + y) as i32,
                    foreground,
                );
            }
        }
    }
}

fn teletext_mosaic_pattern(character: u8) -> Option<u8> {
    match character {
        0x20..=0x3F => Some(character - 0x20),
        0x60..=0x7F => Some((character - 0x40) | 0x20),
        0xA0..=0xBF => Some(character - 0xA0),
        0xE0..=0xFF => Some((character - 0xE0) | 0x20),
        _ => None,
    }
}

fn draw_cursor(snapshot: &GraphicsSnapshot, frame: &mut [u8], width: u32, height: u32) {
    let columns = u32::from(snapshot.mode.text_columns);
    let rows = u32::from(snapshot.mode.text_rows);
    if columns == 0 || rows == 0 {
        return;
    }

    let x = u32::from(snapshot.text_window.left) + snapshot.text_cursor.x.max(0) as u32;
    let y = u32::from(snapshot.text_window.top) + snapshot.text_cursor.y.max(0) as u32;
    if x >= columns || y >= rows {
        return;
    }

    let left = x * width / columns;
    let right = (x + 1) * width / columns;
    let bottom = (y + 1) * height / rows - 1;
    for cursor_x in left..right {
        set_pixel(
            frame,
            width,
            height,
            cursor_x as i32,
            bottom as i32,
            colour(u32::from(snapshot.text_colour), snapshot),
        );
    }
}

fn inside(point: Point, clip: GraphicsWindow, snapshot: &GraphicsSnapshot) -> bool {
    snapshot.mode.graphics_enabled
        && point.x >= clip.left
        && point.x <= clip.right
        && point.y >= clip.bottom
        && point.y <= clip.top
        && point.x >= 0
        && point.x < snapshot.mode.logical_width
        && point.y >= 0
        && point.y < snapshot.mode.logical_height
}

fn clip_line(
    from: Point,
    to: Point,
    clip: GraphicsWindow,
    snapshot: &GraphicsSnapshot,
) -> Option<(Point, Point)> {
    if !snapshot.mode.graphics_enabled {
        return None;
    }
    let left = clip.left.max(0) as f64;
    let right = clip.right.min(snapshot.mode.logical_width - 1) as f64;
    let bottom = clip.bottom.max(0) as f64;
    let top = clip.top.min(snapshot.mode.logical_height - 1) as f64;
    if left > right || bottom > top {
        return None;
    }

    let (mut x0, mut y0) = (f64::from(from.x), f64::from(from.y));
    let (mut x1, mut y1) = (f64::from(to.x), f64::from(to.y));
    loop {
        let code0 = out_code(x0, y0, left, right, bottom, top);
        let code1 = out_code(x1, y1, left, right, bottom, top);
        if code0 | code1 == 0 {
            return Some((
                Point {
                    x: x0.round() as i32,
                    y: y0.round() as i32,
                },
                Point {
                    x: x1.round() as i32,
                    y: y1.round() as i32,
                },
            ));
        }
        if code0 & code1 != 0 {
            return None;
        }

        let outside = if code0 != 0 { code0 } else { code1 };
        let (x, y) = if outside & 8 != 0 {
            if y1 == y0 {
                return None;
            }
            (x0 + (x1 - x0) * (top - y0) / (y1 - y0), top)
        } else if outside & 4 != 0 {
            if y1 == y0 {
                return None;
            }
            (x0 + (x1 - x0) * (bottom - y0) / (y1 - y0), bottom)
        } else if outside & 2 != 0 {
            if x1 == x0 {
                return None;
            }
            (right, y0 + (y1 - y0) * (right - x0) / (x1 - x0))
        } else {
            if x1 == x0 {
                return None;
            }
            (left, y0 + (y1 - y0) * (left - x0) / (x1 - x0))
        };

        if outside == code0 {
            x0 = x;
            y0 = y;
        } else {
            x1 = x;
            y1 = y;
        }
    }
}

fn out_code(x: f64, y: f64, left: f64, right: f64, bottom: f64, top: f64) -> u8 {
    let mut code = 0;
    if x < left {
        code |= 1;
    } else if x > right {
        code |= 2;
    }
    if y < bottom {
        code |= 4;
    } else if y > top {
        code |= 8;
    }
    code
}

fn screen_point(point: Point, snapshot: &GraphicsSnapshot) -> (i32, i32) {
    let width = snapshot.mode.pixel_width;
    let height = snapshot.mode.pixel_height;
    let x = i64::from(point.x) * i64::from(width) / i64::from(snapshot.mode.logical_width);
    let y = i64::from(snapshot.mode.logical_height - 1 - point.y) * i64::from(height)
        / i64::from(snapshot.mode.logical_height);
    (
        x.clamp(0, i64::from(width - 1)) as i32,
        y.clamp(0, i64::from(height - 1)) as i32,
    )
}

fn draw_line(
    frame: &mut [u8],
    width: u32,
    height: u32,
    (mut x0, mut y0): (i32, i32),
    (x1, y1): (i32, i32),
    rgba: [u8; 4],
) {
    let dx = (x1 - x0).abs();
    let sx = if x0 < x1 { 1 } else { -1 };
    let dy = -(y1 - y0).abs();
    let sy = if y0 < y1 { 1 } else { -1 };
    let mut error = dx + dy;

    loop {
        set_pixel(frame, width, height, x0, y0, rgba);
        if x0 == x1 && y0 == y1 {
            break;
        }
        let twice_error = 2 * error;
        if twice_error >= dy {
            error += dy;
            x0 += sx;
        }
        if twice_error <= dx {
            error += dx;
            y0 += sy;
        }
    }
}

fn set_pixel(frame: &mut [u8], width: u32, height: u32, x: i32, y: i32, rgba: [u8; 4]) {
    if x < 0 || y < 0 || x >= width as i32 || y >= height as i32 {
        return;
    }
    let offset = (y as usize * width as usize + x as usize) * BYTES_PER_PIXEL;
    frame[offset..offset + BYTES_PER_PIXEL].copy_from_slice(&rgba);
}

fn colour(logical_colour: u32, snapshot: &GraphicsSnapshot) -> [u8; 4] {
    graphics_colour(logical_colour, snapshot.mode)
}
