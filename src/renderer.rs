//! Rasterizes the hosted BBC screen scene into the shared pixel framebuffer.

use crate::{
    font::bbc_micro_glyph,
    graphics::{GraphicsPrimitive, GraphicsSnapshot, GraphicsWindow, Point, graphics_colour},
    wimp::{DESKTOP_HEIGHT, DESKTOP_WIDTH, DesktopWindow, WorkArea},
};
use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

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

/// Render the hosted two-task Wimp desktop. Application contents use the
/// first-slice task-output adapter documented in the design brief; the window
/// manager still owns all frame, stacking and input routing.
pub fn render_desktop(
    windows: &[DesktopWindow],
    scenes: &HashMap<u64, GraphicsSnapshot>,
    frame: &mut [u8],
) {
    let width = DESKTOP_WIDTH;
    let height = DESKTOP_HEIGHT;
    let bytes = width as usize * height as usize * BYTES_PER_PIXEL;
    if frame.len() < bytes {
        return;
    }
    for pixel in frame[..bytes].chunks_exact_mut(BYTES_PER_PIXEL) {
        pixel.copy_from_slice(&[35, 42, 54, 255]);
    }

    // `windows` is front-to-back; paint from the back towards the front.
    for window in windows.iter().rev() {
        draw_guest_window(window, scenes.get(&window.owner_task_id), frame);
    }
}

fn draw_guest_window(window: &DesktopWindow, scene: Option<&GraphicsSnapshot>, frame: &mut [u8]) {
    let width = DESKTOP_WIDTH;
    let height = DESKTOP_HEIGHT;
    let area = window.work_area;
    let left = area.min_x;
    let right = area.max_x + if window.has_vertical_scrollbar { 14 } else { 0 };
    let top = i32::try_from(height).unwrap_or(i32::MAX)
        - area.max_y
        - if window.has_title { 24 } else { 0 };
    let work_bottom = i32::try_from(height).unwrap_or(i32::MAX) - area.min_y;
    let bottom = work_bottom + if window.resizable { 14 } else { 0 };
    let border = if window.focused {
        [115, 178, 235, 255]
    } else {
        [115, 124, 139, 255]
    };
    fill_rect(frame, width, height, left, top, right, bottom, border);

    let content_left = left + 2;
    let content_right = area.max_x - 2;
    let content_top = i32::try_from(height).unwrap_or(i32::MAX) - area.max_y + 2;
    let content_bottom = i32::try_from(height).unwrap_or(i32::MAX) - area.min_y - 2;
    fill_rect(
        frame,
        width,
        height,
        content_left,
        content_top,
        content_right,
        content_bottom,
        [245, 246, 248, 255],
    );

    if let Some(snapshot) = scene {
        draw_task_scene(
            snapshot,
            frame,
            content_left + 1,
            content_top + 1,
            content_right - 1,
            content_bottom - 1,
        );
    }

    if window.has_title {
        let title_color = if window.focused {
            [45, 102, 157, 255]
        } else {
            [76, 85, 99, 255]
        };
        fill_rect(
            frame,
            width,
            height,
            left + 1,
            top + 1,
            right - 1,
            top + 23,
            title_color,
        );
        draw_small_text_clipped(
            frame,
            width,
            height,
            left + 8,
            top + 8,
            &window.title,
            [255, 255, 255, 255],
            left + 4,
            right - if window.closable { 24 } else { 4 },
            top + 2,
            top + 22,
        );
        if window.closable {
            let close_x = right - 20;
            fill_rect(
                frame,
                width,
                height,
                close_x,
                top + 3,
                right - 3,
                top + 20,
                [217, 224, 232, 255],
            );
            draw_line(
                frame,
                width,
                height,
                (close_x + 6, top + 7),
                (right - 9, top + 16),
                [62, 69, 78, 255],
            );
            draw_line(
                frame,
                width,
                height,
                (close_x + 6, top + 16),
                (right - 9, top + 7),
                [62, 69, 78, 255],
            );
        }
    }

    if window.has_vertical_scrollbar {
        let scroll_left = area.max_x;
        let scroll_right = right;
        let scroll_top = i32::try_from(height).unwrap_or(i32::MAX) - area.max_y;
        fill_rect(
            frame,
            width,
            height,
            scroll_left,
            scroll_top,
            scroll_right,
            work_bottom,
            [206, 211, 219, 255],
        );
        fill_rect(
            frame,
            width,
            height,
            scroll_left + 3,
            scroll_top + 14,
            scroll_right - 3,
            work_bottom - if window.resizable { 14 } else { 0 },
            [229, 232, 237, 255],
        );
        let center_x = (scroll_left + scroll_right) / 2;
        draw_line(
            frame,
            width,
            height,
            (center_x - 3, scroll_top + 7),
            (center_x, scroll_top + 4),
            [80, 89, 101, 255],
        );
        draw_line(
            frame,
            width,
            height,
            (center_x, scroll_top + 4),
            (center_x + 3, scroll_top + 7),
            [80, 89, 101, 255],
        );
        if window.resizable {
            fill_rect(
                frame,
                width,
                height,
                scroll_left,
                work_bottom,
                scroll_right,
                bottom,
                [184, 191, 201, 255],
            );
        }
    }

    if window.resizable {
        let grip_x = area.max_x + 4;
        let grip_y = work_bottom + 4;
        for offset in [0, 4, 8] {
            draw_line(
                frame,
                width,
                height,
                (grip_x + offset, grip_y),
                (right - 3, grip_y + offset),
                [100, 108, 120, 255],
            );
        }
    }

    if let Some(preview) = window.preview_area {
        draw_preview_outline(frame, window, preview);
    }
}

fn draw_task_scene(
    snapshot: &GraphicsSnapshot,
    frame: &mut [u8],
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
) {
    let source_width = snapshot.mode.pixel_width;
    let source_height = snapshot.mode.pixel_height;
    if source_width == 0 || source_height == 0 || right <= left || bottom <= top {
        return;
    }
    let mut source = vec![0; source_width as usize * source_height as usize * BYTES_PER_PIXEL];
    render(snapshot, &mut source);
    let dest_width = (right - left) as u32;
    let dest_height = (bottom - top) as u32;
    let clipped_left = left.max(0);
    let clipped_right = right.min(DESKTOP_WIDTH as i32);
    let clipped_top = top.max(0);
    let clipped_bottom = bottom.min(DESKTOP_HEIGHT as i32);
    for target_y in clipped_top..clipped_bottom {
        let dy = (target_y - top) as u32;
        let sy = dy * source_height / dest_height;
        for target_x in clipped_left..clipped_right {
            let dx = (target_x - left) as u32;
            let sx = dx * source_width / dest_width;
            let source_at = (sy as usize * source_width as usize + sx as usize) * BYTES_PER_PIXEL;
            let target_at =
                (target_y as usize * DESKTOP_WIDTH as usize + target_x as usize) * BYTES_PER_PIXEL;
            frame[target_at..target_at + BYTES_PER_PIXEL]
                .copy_from_slice(&source[source_at..source_at + BYTES_PER_PIXEL]);
        }
    }
}

fn draw_preview_outline(frame: &mut [u8], window: &DesktopWindow, area: WorkArea) {
    let left = area.min_x;
    let right = area.max_x + if window.has_vertical_scrollbar { 14 } else { 0 };
    let top = DESKTOP_HEIGHT as i32 - area.max_y - if window.has_title { 24 } else { 0 };
    let bottom = DESKTOP_HEIGHT as i32 - area.min_y + if window.resizable { 14 } else { 0 };
    let color = [105, 196, 255, 255];
    draw_line(
        frame,
        DESKTOP_WIDTH,
        DESKTOP_HEIGHT,
        (left, top),
        (right, top),
        color,
    );
    draw_line(
        frame,
        DESKTOP_WIDTH,
        DESKTOP_HEIGHT,
        (right, top),
        (right, bottom),
        color,
    );
    draw_line(
        frame,
        DESKTOP_WIDTH,
        DESKTOP_HEIGHT,
        (right, bottom),
        (left, bottom),
        color,
    );
    draw_line(
        frame,
        DESKTOP_WIDTH,
        DESKTOP_HEIGHT,
        (left, bottom),
        (left, top),
        color,
    );
}

fn draw_small_text_clipped(
    frame: &mut [u8],
    width: u32,
    height: u32,
    x: i32,
    y: i32,
    text: &str,
    color: [u8; 4],
    clip_left: i32,
    clip_right: i32,
    clip_top: i32,
    clip_bottom: i32,
) {
    let mut cursor_x = x;
    for byte in text.bytes() {
        if byte == 0 || byte < 32 {
            break;
        }
        if let Some(glyph) = bbc_micro_glyph(byte) {
            for (row, bits) in glyph.iter().enumerate() {
                for column in 0..8 {
                    if bits & (0x80 >> column) != 0 {
                        let pixel_x = cursor_x + column;
                        let pixel_y = y + row as i32;
                        if pixel_x >= clip_left
                            && pixel_x < clip_right
                            && pixel_y >= clip_top
                            && pixel_y < clip_bottom
                        {
                            set_pixel(frame, width, height, pixel_x, pixel_y, color);
                        }
                    }
                }
            }
        }
        cursor_x += 8;
    }
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
