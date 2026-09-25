//! Rasterizes the hosted BBC screen scene into the shared pixel framebuffer.

use crate::{
    font::bbc_micro_glyph,
    graphics::{GraphicsPrimitive, GraphicsSnapshot, GraphicsWindow, Point, graphics_colour},
};

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
                        draw_line(frame, width, height, from, to, colour(*logical_colour));
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
                        set_pixel(frame, width, height, x, y, colour(*logical_colour));
                    }
                }
            }
        }
    }

    draw_text(snapshot, frame, width, height);
    draw_cursor(snapshot, frame, width, height);
}

fn draw_text(snapshot: &GraphicsSnapshot, frame: &mut [u8], width: u32, height: u32) {
    let columns = u32::from(snapshot.mode.text_columns);
    let rows = u32::from(snapshot.mode.text_rows);
    if columns == 0 || rows == 0 {
        return;
    }

    let text_colour = colour(u32::from(snapshot.text_colour));

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
            colour(u32::from(snapshot.text_colour)),
        );
    }
}

fn inside(point: Point, clip: GraphicsWindow, snapshot: &GraphicsSnapshot) -> bool {
    point.x >= clip.left
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

fn colour(logical_colour: u32) -> [u8; 4] {
    graphics_colour(logical_colour)
}
