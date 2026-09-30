//! Rasterizes the hosted BBC screen scene into the shared pixel framebuffer.

use crate::{
    font::bbc_micro_glyph,
    graphics::{
        GraphicsPrimitive, GraphicsSnapshot, GraphicsWindow, Point, TextRenderingProfile,
        graphics_colour, logical_rect_pixels,
    },
    riscos_font::NativeRasterFont,
    riscos_resources::{FontName, RiscOsSprite, RiscOsSpriteFile, SpriteSet, builtin_sprite_set},
    wimp::{
        DESKTOP_HEIGHT, DESKTOP_ICONBAR_HEIGHT, DESKTOP_OS_UNITS_PER_PIXEL_X,
        DESKTOP_OS_UNITS_PER_PIXEL_Y, DESKTOP_PIXEL_HEIGHT, DESKTOP_PIXEL_WIDTH, DesktopIcon,
        DesktopIconImage, DesktopMenu, DesktopRect, DesktopWindow, DesktopWindowIcon, FRAME_BORDER,
        ICONBAR_SYSTEM_AREA_OS, MENU_SEPARATOR_HEIGHT, VerticalScrollbarLayout,
        WindowFurnitureLayout, WorkArea,
    },
};
use std::time::{SystemTime, UNIX_EPOCH};
use std::{
    collections::HashMap,
    io::Cursor,
    sync::{Arc, Mutex, OnceLock},
};

pub const SCREEN_WIDTH: u32 = 640;
pub const SCREEN_HEIGHT: u32 = 256;
const BYTES_PER_PIXEL: usize = 4;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SoftwareRenderError {
    ModernTextRequiresVello,
}

impl std::fmt::Display for SoftwareRenderError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ModernTextRequiresVello => formatter.write_str(
                "TEXT=MODERN text requires the Parley/Vello compositor; software rendering supports TEXT=CLASSIC only",
            ),
        }
    }
}

impl std::error::Error for SoftwareRenderError {}

/// Paint a complete Classic graphics/text snapshot into an RGBA frame.
/// Modern text must use the Parley/Vello compositor and returns an error here.
pub fn render(snapshot: &GraphicsSnapshot, frame: &mut [u8]) -> Result<(), SoftwareRenderError> {
    if snapshot.text_profile == TextRenderingProfile::Modern {
        return Err(SoftwareRenderError::ModernTextRequiresVello);
    }
    render_for_vello(snapshot, frame);
    Ok(())
}

/// Render the raster and any Classic bitmap text for a Vello scene. Modern
/// text is intentionally omitted here because the Vello scene overlays it.
pub fn render_for_vello(snapshot: &GraphicsSnapshot, frame: &mut [u8]) {
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
                GraphicsPrimitive::ClearRectangle {
                    bounds,
                    logical_colour,
                } => {
                    let (left, top, right, bottom) = logical_rect_pixels(*bounds, snapshot);
                    fill_rect(
                        frame,
                        width,
                        height,
                        left as i32,
                        top as i32,
                        right as i32,
                        bottom as i32,
                        colour(*logical_colour, snapshot),
                    );
                }
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

    if snapshot.text_profile == TextRenderingProfile::Classic {
        draw_text(snapshot, frame, width, height);
        draw_cursor(snapshot, frame, width, height);
    }
}

/// Render guest content for a software desktop snapshot. Modern text cannot
/// be rasterized by this path and is rejected explicitly.
pub fn render_desktop_content(
    snapshot: &GraphicsSnapshot,
    frame: &mut [u8],
) -> Result<(), SoftwareRenderError> {
    // Guest VDU output has identical bitmap glyphs, palette and MODE semantics
    // in a Wimp window and fullscreen. Only the host furniture uses UI fonts.
    render(snapshot, frame)
}

/// Render only the compatibility image that the Vello compositor uses as its
/// guest surface; Modern text is drawn separately by the compositor.
pub fn render_desktop_content_for_vello(snapshot: &GraphicsSnapshot, frame: &mut [u8]) {
    render_for_vello(snapshot, frame);
}

/// Render the hosted Wimp desktop. Each guest Wimp window has its own
/// compatibility content snapshot; only host-created BASIC output windows
/// display the task-default destination.
pub fn render_desktop(
    windows: &[DesktopWindow],
    scenes: &HashMap<(u64, Option<u32>), GraphicsSnapshot>,
    icons: &[DesktopIcon],
    window_icons: &[DesktopWindowIcon],
    menus: &[DesktopMenu],
    notice: Option<&str>,
    frame: &mut [u8],
) -> Result<(), SoftwareRenderError> {
    let width = DESKTOP_PIXEL_WIDTH;
    let height = DESKTOP_PIXEL_HEIGHT;
    let bytes = width as usize * height as usize * BYTES_PER_PIXEL;
    if frame.len() < bytes {
        return Ok(());
    }
    if scenes
        .values()
        .any(|snapshot| snapshot.text_profile == TextRenderingProfile::Modern)
    {
        return Err(SoftwareRenderError::ModernTextRequiresVello);
    }
    draw_desktop_background(frame);

    // `windows` is front-to-back; paint from the back towards the front.
    for window in windows.iter().rev() {
        draw_guest_window(
            window,
            scenes
                .get(&(window.owner_task_id, Some(window.handle)))
                .or_else(|| {
                    window
                        .is_console_output
                        .then(|| scenes.get(&(window.owner_task_id, None)))
                        .flatten()
                }),
            window_icons,
            frame,
        );
    }
    let frontmost_task = windows.first().map(|window| window.owner_task_id);
    draw_icon_bar(icons, frontmost_task, frame);
    if let Some(notice) = notice {
        draw_desktop_notice(notice, frame);
    }
    // Menu snapshots run from root to deepest child; children paint on top.
    for menu in menus {
        draw_menu(frame, menu);
    }
    Ok(())
}

fn menu_colour(index: u8) -> [u8; 4] {
    const PALETTE: [[u8; 3]; 16] = [
        [255, 255, 255],
        [221, 221, 221],
        [187, 187, 187],
        [153, 153, 153],
        [119, 119, 119],
        [85, 85, 85],
        [51, 51, 51],
        [0, 0, 0],
        [0, 68, 153],
        [238, 238, 0],
        [0, 204, 0],
        [221, 0, 0],
        [238, 238, 187],
        [85, 136, 0],
        [255, 187, 0],
        [0, 187, 255],
    ];
    let [r, g, b] = PALETTE[(index & 15) as usize];
    [r, g, b, 255]
}

fn draw_menu(frame: &mut [u8], menu: &DesktopMenu) {
    let bounds = to_pixel_rect(menu.bounds);
    let ink = menu_colour(menu.work_foreground);
    let paper = menu_colour(menu.work_background);
    let fill = |frame: &mut [u8], rect: PixelRect, color| {
        fill_rect(
            frame,
            DESKTOP_PIXEL_WIDTH,
            DESKTOP_PIXEL_HEIGHT,
            rect.left,
            rect.top,
            rect.right,
            rect.bottom,
            color,
        );
    };
    fill(frame, bounds, paper);
    if let Some(title) = menu.title_bounds {
        let title = to_pixel_rect(title);
        fill(frame, title, menu_colour(menu.title_background));
        let width = native_ui_font(20).measure_text_px(&menu.title).round() as i32;
        draw_outline_text(
            frame,
            (title.left + title.right - width) / 2,
            (title.top + title.bottom) / 2 + 7,
            &menu.title,
            title,
            20,
            menu_colour(menu.title_foreground),
            false,
        );
        fill(
            frame,
            PixelRect {
                top: title.bottom - FRAME_BORDER,
                ..title
            },
            [0, 0, 0, 255],
        );
    }
    for item in &menu.rows {
        let row = to_pixel_rect(item.bounds);
        let band = PixelRect {
            left: row.left + 24,
            right: row.right - 24,
            ..row
        };
        let selected = item.selected && !item.shaded;
        if selected {
            fill(frame, band, ink);
        }
        let text_ink = if item.shaded {
            [136, 136, 136, 255]
        } else if selected {
            paper
        } else {
            ink
        };
        let text_width = native_ui_font(20).measure_text_px(&item.label).round() as i32;
        let text_x = if menu.reverse {
            band.right - text_width - 5
        } else {
            band.left + 5
        };
        draw_outline_text(
            frame,
            text_x,
            (row.top + row.bottom) / 2 + 7,
            &item.label,
            band,
            20,
            text_ink,
            false,
        );
        let center_y = (row.top + row.bottom) / 2;
        let ornament_ink = if item.shaded {
            [170, 170, 170, 255]
        } else {
            ink
        };
        if item.tick {
            let x = if menu.reverse {
                row.right - 21
            } else {
                row.left + 5
            };
            draw_control_line(
                frame,
                DESKTOP_PIXEL_WIDTH,
                DESKTOP_PIXEL_HEIGHT,
                (x, center_y),
                (x + 5, center_y + 5),
                ornament_ink,
            );
            draw_control_line(
                frame,
                DESKTOP_PIXEL_WIDTH,
                DESKTOP_PIXEL_HEIGHT,
                (x + 5, center_y + 5),
                (x + 15, center_y - 7),
                ornament_ink,
            );
        }
        if item.has_submenu {
            let x = if menu.reverse {
                row.left + 16
            } else {
                row.right - 17
            };
            for offset in 0i32..9 {
                let px = if menu.reverse { x - offset } else { x + offset };
                fill(
                    frame,
                    PixelRect {
                        left: px,
                        right: px + 1,
                        top: center_y - (8 - offset),
                        bottom: center_y + (8 - offset) + 1,
                    },
                    ornament_ink,
                );
            }
        }
        if item.separator_after {
            let mut x = row.left + FRAME_BORDER;
            while x < row.right - FRAME_BORDER {
                fill(
                    frame,
                    PixelRect {
                        left: x,
                        right: (x + 6).min(row.right - FRAME_BORDER),
                        top: row.bottom + MENU_SEPARATOR_HEIGHT / 2 - FRAME_BORDER / 2,
                        bottom: row.bottom + MENU_SEPARATOR_HEIGHT / 2 + FRAME_BORDER / 2,
                    },
                    ink,
                );
                x += 12;
            }
        }
    }
    draw_frame_outline(frame, bounds);
}

fn draw_icon_bar(icons: &[DesktopIcon], frontmost_task: Option<u64>, frame: &mut [u8]) {
    let top = DESKTOP_PIXEL_HEIGHT as i32 - DESKTOP_ICONBAR_HEIGHT / DESKTOP_OS_UNITS_PER_PIXEL_Y;
    fill_rect(
        frame,
        DESKTOP_PIXEL_WIDTH,
        DESKTOP_PIXEL_HEIGHT,
        0,
        top,
        DESKTOP_PIXEL_WIDTH as i32,
        DESKTOP_PIXEL_HEIGHT as i32,
        [224, 224, 224, 255],
    );
    fill_rect(
        frame,
        DESKTOP_PIXEL_WIDTH,
        DESKTOP_PIXEL_HEIGHT,
        0,
        top,
        DESKTOP_PIXEL_WIDTH as i32,
        top + FRAME_BORDER,
        [59, 59, 59, 255],
    );
    for icon in icons {
        let bounds = to_pixel_rect(icon.bounds);
        let artwork_clip = PixelRect {
            left: bounds.left,
            top: top + 4,
            right: bounds.right,
            bottom: top + 68,
        };
        if let Some(image) = &icon.high_resolution_image {
            let x = bounds.left + (bounds.right - bounds.left - image.width as i32) / 2;
            let y = top + 6 + (64 - image.height as i32) / 2;
            draw_rgba_icon_image(frame, image, x, y, artwork_clip, false);
        } else if icon.sprite_name.as_deref() == Some("application") {
            draw_modern_task_icon(frame, bounds, top);
        } else if let Some(sprite) = icon
            .sprite_name
            .as_deref()
            .and_then(|name| desktop_system_sprites().get(name))
        {
            let sprite_width = sprite.width as i32 * 2;
            let sprite_height = sprite.height as i32 * 2;
            let sprite_x = bounds.left + ((bounds.right - bounds.left - sprite_width) / 2).max(0);
            let sprite_y = top + 6 + ((64 - sprite_height) / 2).max(0);
            draw_sprite_at_scaled(frame, sprite, sprite_x, sprite_y, 2);
        }
        draw_outline_text(
            frame,
            (bounds.left + bounds.right) / 2,
            top + 88,
            &icon.label,
            PixelRect {
                left: bounds.left + 8,
                top: top + 68,
                right: bounds.right - 8,
                bottom: DESKTOP_PIXEL_HEIGHT as i32 - 6,
            },
            22,
            [0, 0, 0, 255],
            true,
        );
        if frontmost_task.is_some() && icon.activate_task_id == frontmost_task {
            let center = (bounds.left + bounds.right) / 2;
            fill_rect(
                frame,
                DESKTOP_PIXEL_WIDTH,
                DESKTOP_PIXEL_HEIGHT,
                center - 20,
                DESKTOP_PIXEL_HEIGHT as i32 - 5,
                center + 20,
                DESKTOP_PIXEL_HEIGHT as i32 - 2,
                [37, 103, 202, 255],
            );
        }
    }

    let icon_size = 64;
    let icon_left = DESKTOP_PIXEL_WIDTH as i32 - ICONBAR_SYSTEM_AREA_OS;
    let icon_top = top
        + (DESKTOP_ICONBAR_HEIGHT / DESKTOP_OS_UNITS_PER_PIXEL_Y - icon_size) / 2;
    draw_rgba_icon_fit(
        frame,
        os_icon_image(),
        icon_left,
        icon_top,
        icon_size,
        PixelRect {
            left: icon_left,
            top: icon_top,
            right: icon_left + icon_size,
            bottom: icon_top + icon_size,
        },
        false,
    );
}

fn os_icon_image() -> &'static DesktopIconImage {
    static IMAGE: OnceLock<DesktopIconImage> = OnceLock::new();
    IMAGE.get_or_init(|| {
        let decoder = png::Decoder::new(Cursor::new(include_bytes!(
            "../resources/branding/desktop-flat/OSIcon.png"
        )));
        let mut reader = decoder
            .read_info()
            .expect("bundled OS icon PNG has valid metadata");
        let mut rgba = vec![0; reader.output_buffer_size().expect("OS icon size is bounded")];
        let info = reader
            .next_frame(&mut rgba)
            .expect("bundled OS icon PNG has valid pixels");
        assert_eq!(info.color_type, png::ColorType::Rgba);
        assert_eq!(info.bit_depth, png::BitDepth::Eight);
        rgba.truncate(info.buffer_size());
        let rgba = rgba
            .chunks_exact(4)
            .map(|pixel| [pixel[0], pixel[1], pixel[2], pixel[3]])
            .collect();
        DesktopIconImage {
            width: info.width,
            height: info.height,
            rgba,
        }
    })
}

fn draw_modern_task_icon(frame: &mut [u8], bounds: PixelRect, bar_top: i32) {
    let size = 48;
    let left = bounds.left + (bounds.right - bounds.left - size) / 2;
    let top = bar_top + 6 + (64 - size) / 2;
    let tile = PixelRect {
        left,
        top,
        right: left + size,
        bottom: top + size,
    };
    fill_rect(
        frame,
        DESKTOP_PIXEL_WIDTH,
        DESKTOP_PIXEL_HEIGHT,
        tile.left,
        tile.top,
        tile.right,
        tile.bottom,
        [245, 245, 245, 255],
    );
    draw_rect_outline(frame, tile, [20, 20, 20, 255]);
    fill_rect(
        frame,
        DESKTOP_PIXEL_WIDTH,
        DESKTOP_PIXEL_HEIGHT,
        tile.left + 8,
        tile.top + 9,
        tile.left + 11,
        tile.bottom - 8,
        [39, 102, 197, 255],
    );
    for (line, right) in [
        (17, tile.right - 8),
        (25, tile.right - 12),
        (33, tile.right - 8),
    ] {
        fill_rect(
            frame,
            DESKTOP_PIXEL_WIDTH,
            DESKTOP_PIXEL_HEIGHT,
            tile.left + 18,
            tile.top + line,
            right,
            tile.top + line + 2,
            [70, 70, 70, 255],
        );
    }
    fill_rect(
        frame,
        DESKTOP_PIXEL_WIDTH,
        DESKTOP_PIXEL_HEIGHT,
        tile.right - 13,
        tile.top + 7,
        tile.right - 7,
        tile.top + 13,
        [218, 176, 34, 255],
    );
}

fn draw_desktop_background(frame: &mut [u8]) {
    let bytes = DESKTOP_PIXEL_WIDTH as usize * DESKTOP_PIXEL_HEIGHT as usize * BYTES_PER_PIXEL;
    for pixel in frame[..bytes].chunks_exact_mut(BYTES_PER_PIXEL) {
        pixel.copy_from_slice(&[185, 185, 187, 255]);
    }
}

fn draw_desktop_notice(text: &str, frame: &mut [u8]) {
    let left = 240;
    let top = 484;
    let bounds = PixelRect {
        left,
        top,
        right: DESKTOP_PIXEL_WIDTH as i32 - left,
        bottom: top + 216,
    };
    fill_rect(
        frame,
        DESKTOP_PIXEL_WIDTH,
        DESKTOP_PIXEL_HEIGHT,
        bounds.left,
        bounds.top,
        bounds.right,
        bounds.bottom,
        [238, 238, 238, 255],
    );
    draw_frame_outline(frame, bounds);
    draw_outline_text(
        frame,
        bounds.left + 44,
        bounds.top + 62,
        "System message",
        PixelRect {
            left: bounds.left + 36,
            top: bounds.top + 16,
            right: bounds.right - 32,
            bottom: bounds.top + 76,
        },
        32,
        [20, 20, 20, 255],
        false,
    );
    draw_outline_text(
        frame,
        bounds.left + 44,
        bounds.top + 140,
        text,
        PixelRect {
            left: bounds.left + 36,
            top: bounds.top + 92,
            right: bounds.right - 32,
            bottom: bounds.bottom - 20,
        },
        28,
        [20, 20, 20, 255],
        false,
    );
}

fn draw_guest_window(
    window: &DesktopWindow,
    scene: Option<&GraphicsSnapshot>,
    icons: &[DesktopWindowIcon],
    frame: &mut [u8],
) {
    let width = DESKTOP_PIXEL_WIDTH;
    let height = DESKTOP_PIXEL_HEIGHT;
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
        [228, 228, 229, 255],
    );
    draw_frame_outline(frame, outer);
    let work_background = if window.title.starts_with("HostFS:")
        || window.title.to_ascii_lowercase().contains("filer")
    {
        [239, 239, 239, 255]
    } else {
        [255, 255, 255, 255]
    };
    fill_rect(
        frame,
        width,
        height,
        work.left,
        work.top,
        work.right,
        work.bottom,
        work_background,
    );

    if let Some(snapshot) = scene.filter(|snapshot| snapshot_has_drawable_content(snapshot)) {
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

    for icon in icons
        .iter()
        .filter(|icon| icon.window_handle == window.handle)
    {
        draw_window_icon(frame, work, icon);
    }

    if window.has_title {
        draw_title_bar(frame, current, window);
    }
    if let Some(scrollbar) = current.vertical_scrollbar {
        draw_vertical_scrollbar(frame, scrollbar);
    }
    if let Some(icon) = current.adjust_size_icon {
        fill_rect(
            frame,
            width,
            height,
            outer.left,
            work.bottom,
            outer.right,
            work.bottom + FRAME_BORDER,
            [59, 59, 59, 255],
        );
        draw_size_grip(frame, icon);
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
    render_desktop_content(snapshot, &mut source)
        .expect("render_desktop rejects Modern snapshots before drawing guest windows");

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

    fn intersection(self, other: Self) -> Option<Self> {
        let rect = Self {
            left: self.left.max(other.left),
            top: self.top.max(other.top),
            right: self.right.min(other.right),
            bottom: self.bottom.min(other.bottom),
        };
        (!rect.is_empty()).then_some(rect)
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

fn desktop_system_sprites() -> &'static RiscOsSpriteFile {
    static SPRITES: OnceLock<RiscOsSpriteFile> = OnceLock::new();
    SPRITES.get_or_init(|| {
        builtin_sprite_set(SpriteSet::Sprites22)
            .expect("pinned RISC OS 3.71 system sprites parse and verify")
    })
}

pub(crate) fn desktop_system_sprite(name: &str) -> Option<&'static RiscOsSprite> {
    let name = if name == "file_064" { "file_ffb" } else { name };
    desktop_system_sprites().get(name)
}

// Two backing samples equal one logical pixel at the desktop’s 2× density.
fn draw_control_line(
    frame: &mut [u8],
    width: u32,
    height: u32,
    from: (i32, i32),
    to: (i32, i32),
    color: [u8; 4],
) {
    draw_line(frame, width, height, from, to, color);
    draw_line(
        frame,
        width,
        height,
        (from.0 + 1, from.1),
        (to.0 + 1, to.1),
        color,
    );
}

fn draw_symbol_outline(frame: &mut [u8], rect: PixelRect, color: [u8; 4]) {
    for inset in 0..FRAME_BORDER {
        draw_rect_outline(
            frame,
            PixelRect {
                left: rect.left + inset,
                top: rect.top + inset,
                right: rect.right - inset,
                bottom: rect.bottom - inset,
            },
            color,
        );
    }
}

fn draw_frame_outline(frame: &mut [u8], rect: PixelRect) {
    for inset in 0..FRAME_BORDER {
        draw_rect_outline(
            frame,
            PixelRect {
                left: rect.left + inset,
                top: rect.top + inset,
                right: rect.right - inset,
                bottom: rect.bottom - inset,
            },
            [59, 59, 59, 255],
        );
    }
}

fn draw_rect_outline(frame: &mut [u8], rect: PixelRect, color: [u8; 4]) {
    if rect.is_empty() {
        return;
    }
    draw_line(
        frame,
        DESKTOP_PIXEL_WIDTH,
        DESKTOP_PIXEL_HEIGHT,
        (rect.left, rect.top),
        (rect.right - 1, rect.top),
        color,
    );
    draw_line(
        frame,
        DESKTOP_PIXEL_WIDTH,
        DESKTOP_PIXEL_HEIGHT,
        (rect.left, rect.top),
        (rect.left, rect.bottom - 1),
        color,
    );
    draw_line(
        frame,
        DESKTOP_PIXEL_WIDTH,
        DESKTOP_PIXEL_HEIGHT,
        (rect.left, rect.bottom - 1),
        (rect.right - 1, rect.bottom - 1),
        color,
    );
    draw_line(
        frame,
        DESKTOP_PIXEL_WIDTH,
        DESKTOP_PIXEL_HEIGHT,
        (rect.right - 1, rect.top),
        (rect.right - 1, rect.bottom - 1),
        color,
    );
}

fn blend_pixel(frame: &mut [u8], x: i32, y: i32, color: [u8; 4]) {
    if x < 0 || y < 0 || x >= DESKTOP_PIXEL_WIDTH as i32 || y >= DESKTOP_PIXEL_HEIGHT as i32 {
        return;
    }
    let index = (y as usize * DESKTOP_PIXEL_WIDTH as usize + x as usize) * BYTES_PER_PIXEL;
    let alpha = u32::from(color[3]);
    let inverse = 255 - alpha;
    for (channel, source) in color[..3].iter().enumerate() {
        frame[index + channel] = ((u32::from(*source) * alpha
            + u32::from(frame[index + channel]) * inverse
            + 127)
            / 255) as u8;
    }
    frame[index + 3] = 255;
}

fn snapshot_has_drawable_content(snapshot: &GraphicsSnapshot) -> bool {
    snapshot.raster_surface.is_some()
        || !snapshot.primitives.is_empty()
        || snapshot
            .text_cells
            .iter()
            .any(|character| *character != b' ')
}

// The hosted menu uses the standard Wimp greyscale colour fields. Existing
// icons with no explicit colour word retain their modern desktop palette.
pub(crate) fn window_icon_colours(flags: u32) -> ([u8; 4], [u8; 4]) {
    let foreground = (flags >> 24) & 15;
    let background = (flags >> 28) & 15;
    let explicit_grey =
        flags & (1 << 6) == 0 && flags >> 24 != 0 && foreground < 8 && background < 8;
    let grey = |index: u32| {
        let value = [255, 221, 187, 153, 119, 85, 51, 0][index as usize];
        [value, value, value, 255]
    };
    let (mut ink, mut paper) = if explicit_grey {
        (grey(foreground), grey(background))
    } else {
        ([20, 20, 20, 255], [248, 248, 248, 255])
    };
    let shaded = flags & (1 << 22) != 0;
    if flags & (1 << 21) != 0 && !shaded {
        if explicit_grey {
            std::mem::swap(&mut ink, &mut paper);
        } else {
            ink = [255, 255, 255, 255];
            paper = [65, 106, 228, 255];
        }
    }
    if shaded {
        for channel in 0..3 {
            ink[channel] = ((u16::from(ink[channel]) + u16::from(paper[channel]) * 2) / 3) as u8;
        }
    }
    (ink, paper)
}

fn draw_window_icon(frame: &mut [u8], work: PixelRect, icon: &DesktopWindowIcon) {
    let bounds = to_pixel_rect(icon.bounds);
    let Some(visible) = bounds.intersection(work) else {
        return;
    };
    let selected = icon.flags & (1 << 21) != 0 && icon.flags & (1 << 22) == 0;
    let (ink, paper) = window_icon_colours(icon.flags);
    let filled = icon.flags & (1 << 5) != 0;
    let pictorial = icon.high_resolution_image.is_some() || icon.sprite_name.is_some();
    if filled || (selected && !pictorial) {
        fill_rect(
            frame,
            DESKTOP_PIXEL_WIDTH,
            DESKTOP_PIXEL_HEIGHT,
            visible.left,
            visible.top,
            visible.right,
            visible.bottom,
            paper,
        );
    }
    if icon.flags & (1 << 2) != 0 && visible == bounds {
        draw_rect_outline(frame, bounds, [24, 24, 24, 255]);
    }

    let sprite = icon
        .sprite_name
        .as_deref()
        .and_then(|name| desktop_system_sprites().get(name));
    let horizontal = pictorial && icon.flags & (1 << 3) == 0;
    let compact_size = (bounds.bottom - bounds.top - 8).clamp(1, 28);
    let label_height = if icon.label.is_empty() { 0 } else { 36 };
    if let Some(image) = &icon.high_resolution_image {
        if horizontal {
            draw_rgba_icon_fit(
                frame,
                image,
                bounds.left + 8,
                (bounds.top + bounds.bottom - compact_size) / 2,
                compact_size,
                visible,
                selected,
            );
        } else {
            let area_bottom = bounds.bottom - label_height;
            let x = bounds.left + (bounds.right - bounds.left - image.width as i32) / 2;
            let y = bounds.top + (area_bottom - bounds.top - image.height as i32) / 2;
            draw_rgba_icon_image(frame, image, x, y, visible, selected);
        }
    } else if let Some(sprite) = sprite {
        let area_bottom = bounds.bottom - label_height;
        let sprite_width = sprite.width as i32 * 2;
        let sprite_height = sprite.height as i32 * 2;
        let x = bounds.left + ((bounds.right - bounds.left - sprite_width) / 2).max(0);
        let y = bounds.top + ((area_bottom - bounds.top - sprite_height) / 2).max(0);
        if selected {
            draw_sprite_clipped_scaled(frame, sprite, x, y, visible, true, 2);
        } else {
            draw_sprite_clipped_scaled(frame, sprite, x, y, visible, false, 2);
        }
    }
    if !icon.label.is_empty() {
        let font = native_ui_font(20);
        let text_width = font.measure_text_px(&icon.label).round() as i32;
        let centered = icon.flags & (1 << 3) != 0;
        let x = if icon.flags & (1 << 9) != 0 {
            bounds.right - 10 - text_width
        } else if centered {
            bounds.left + (bounds.right - bounds.left - text_width) / 2
        } else if horizontal {
            bounds.left + compact_size + 16
        } else {
            bounds.left + 10
        };
        let text_only = icon.high_resolution_image.is_none() && sprite.is_none();
        let baseline = if (text_only || horizontal) && icon.flags & (1 << 4) != 0 {
            (bounds.top + bounds.bottom) / 2 + 7
        } else {
            bounds.bottom - 6
        };
        let label_top = baseline - 22;
        if selected && pictorial {
            if let Some(label) = visible.intersection(PixelRect {
                left: x - 3,
                top: label_top,
                right: x + text_width + 3,
                bottom: baseline + 5,
            }) {
                fill_rect(
                    frame,
                    DESKTOP_PIXEL_WIDTH,
                    DESKTOP_PIXEL_HEIGHT,
                    label.left,
                    label.top,
                    label.right,
                    label.bottom,
                    [0, 0, 0, 255],
                );
            }
        }
        draw_outline_text(
            frame,
            x,
            baseline,
            &icon.label,
            visible
                .intersection(PixelRect {
                    left: bounds.left + 1,
                    top: label_top,
                    right: bounds.right - 1,
                    bottom: bounds.bottom,
                })
                .unwrap_or(PixelRect {
                    left: 0,
                    top: 0,
                    right: 0,
                    bottom: 0,
                }),
            20,
            ink,
            false,
        );
    }
}

fn draw_title_bar(frame: &mut [u8], layout: WindowFurnitureLayout, window: &DesktopWindow) {
    let header = DesktopRect {
        min_x: layout.outer.min_x,
        min_y: layout.work_area.max_y,
        max_x: layout.outer.max_x,
        max_y: layout.outer.max_y,
    };
    let band = to_pixel_rect(header);
    if band.is_empty() {
        return;
    }
    let active = window.focused;
    let title_color = if active {
        [239, 220, 103, 255]
    } else {
        [213, 213, 215, 255]
    };
    fill_rect(
        frame,
        DESKTOP_PIXEL_WIDTH,
        DESKTOP_PIXEL_HEIGHT,
        band.left,
        band.top,
        band.right,
        band.bottom,
        title_color,
    );
    draw_frame_outline(frame, band);

    if let Some(icon) = layout.back_icon {
        draw_title_control(frame, icon, TitleControl::Back, active);
    }
    if let Some(icon) = layout.close_icon {
        draw_title_control(frame, icon, TitleControl::Close, active);
    }
    if let Some(icon) = layout.toggle_size_icon {
        draw_title_control(frame, icon, TitleControl::Toggle(window.maximized), active);
    }
    if !window.title.is_empty() {
        if let Some(title) = layout.title_bar {
            let rect = to_pixel_rect(title);
            let baseline = band.top + (band.bottom - band.top + 24) / 2;
            // A one-sample emboldening keeps the original Homerton face while
            // giving window titles the reference's stronger visual hierarchy.
            for offset in 0..=1 {
                draw_outline_text(
                    frame,
                    rect.left + 14 + offset,
                    baseline,
                    &window.title,
                    rect,
                    24,
                    [24, 24, 24, 255],
                    false,
                );
            }
        }
    }
}

#[derive(Clone, Copy)]
enum TitleControl {
    Back,
    Close,
    Toggle(bool),
}

fn draw_title_control(frame: &mut [u8], cell: DesktopRect, control: TitleControl, _active: bool) {
    let rect = to_pixel_rect(cell);
    // The title band owns the perimeter; each control owns only its divider.
    let divider = match control {
        TitleControl::Toggle(_) => rect.left,
        _ => rect.right - FRAME_BORDER,
    };
    fill_rect(
        frame,
        DESKTOP_PIXEL_WIDTH,
        DESKTOP_PIXEL_HEIGHT,
        divider,
        rect.top,
        divider + FRAME_BORDER,
        rect.bottom,
        [59, 59, 59, 255],
    );
    let ink = [20, 20, 20, 255];
    let center_x = (rect.left + rect.right) / 2;
    let center_y = (rect.top + rect.bottom) / 2;
    match control {
        TitleControl::Close => {
            draw_control_line(
                frame,
                DESKTOP_PIXEL_WIDTH,
                DESKTOP_PIXEL_HEIGHT,
                (center_x - 8, center_y - 8),
                (center_x + 8, center_y + 8),
                ink,
            );
            draw_control_line(
                frame,
                DESKTOP_PIXEL_WIDTH,
                DESKTOP_PIXEL_HEIGHT,
                (center_x + 8, center_y - 8),
                (center_x - 8, center_y + 8),
                ink,
            );
        }
        TitleControl::Back => {
            draw_symbol_outline(
                frame,
                PixelRect {
                    left: center_x - 11,
                    top: center_y - 9,
                    right: center_x + 5,
                    bottom: center_y + 7,
                },
                ink,
            );
            draw_symbol_outline(
                frame,
                PixelRect {
                    left: center_x - 5,
                    top: center_y - 3,
                    right: center_x + 11,
                    bottom: center_y + 13,
                },
                ink,
            );
        }
        TitleControl::Toggle(maximized) => {
            let side = 18;
            let square = PixelRect {
                left: center_x - side / 2,
                top: center_y - side / 2,
                right: center_x + side / 2 + 1,
                bottom: center_y + side / 2 + 1,
            };
            draw_symbol_outline(frame, square, ink);
            if maximized {
                draw_control_line(
                    frame,
                    DESKTOP_PIXEL_WIDTH,
                    DESKTOP_PIXEL_HEIGHT,
                    (square.left + 6, square.top + 6),
                    (square.right - 6, square.top + 6),
                    ink,
                );
            }
        }
    }
}

fn draw_outline_text(
    frame: &mut [u8],
    x: i32,
    baseline_y: i32,
    text: &str,
    clip: PixelRect,
    pixel_size: u16,
    color: [u8; 4],
    centered: bool,
) {
    let font = native_ui_font(pixel_size);
    let measured_width = font.measure_text_px(text).round() as i32;
    let mut pen_x = if centered { x - measured_width / 2 } else { x };
    for character in text.chars() {
        let glyph = font
            .rasterize_glyph(character)
            .or_else(|| font.rasterize_glyph('?'));
        if let Some(glyph) = glyph {
            let left = pen_x + glyph.bearing_x;
            let top = baseline_y - glyph.bearing_y;
            for row in 0..glyph.height as i32 {
                let py = top + row;
                if py < clip.top || py >= clip.bottom {
                    continue;
                }
                for column in 0..glyph.width as i32 {
                    let px = left + column;
                    if px < clip.left || px >= clip.right {
                        continue;
                    }
                    let coverage =
                        glyph.coverage[(row as usize * glyph.width as usize) + column as usize];
                    if coverage > 0 {
                        blend_glyph_pixel(
                            frame,
                            DESKTOP_PIXEL_WIDTH,
                            DESKTOP_PIXEL_HEIGHT,
                            px,
                            py,
                            color,
                            coverage,
                        );
                    }
                }
            }
        }
        pen_x += font.advance_px(character).round() as i32;
    }
}

fn native_ui_font(pixel_size: u16) -> Arc<NativeRasterFont> {
    static FONTS: OnceLock<Mutex<HashMap<u16, Arc<NativeRasterFont>>>> = OnceLock::new();
    let mut fonts = FONTS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    Arc::clone(fonts.entry(pixel_size).or_insert_with(|| {
        Arc::new(
            NativeRasterFont::builtin(FontName::Homerton, pixel_size)
                .expect("pinned Homerton outline font parses and verifies"),
        )
    }))
}

fn blend_glyph_pixel(
    frame: &mut [u8],
    width: u32,
    height: u32,
    x: i32,
    y: i32,
    color: [u8; 4],
    coverage: u8,
) {
    if x < 0 || y < 0 || x >= width as i32 || y >= height as i32 {
        return;
    }
    let index = (y as usize * width as usize + x as usize) * BYTES_PER_PIXEL;
    let alpha = u32::from(coverage) * u32::from(color[3]) / 255;
    let inverse = 255 - alpha;
    for (channel, source) in color[..3].iter().enumerate() {
        frame[index + channel] = ((u32::from(*source) * alpha
            + u32::from(frame[index + channel]) * inverse
            + 127)
            / 255) as u8;
    }
    frame[index + 3] = 255;
}

fn draw_vertical_scrollbar(frame: &mut [u8], layout: VerticalScrollbarLayout) {
    let bounds = to_pixel_rect(layout.bounds);
    fill_rect(
        frame,
        DESKTOP_PIXEL_WIDTH,
        DESKTOP_PIXEL_HEIGHT,
        bounds.left,
        bounds.top,
        bounds.right,
        bounds.bottom,
        [224, 224, 224, 255],
    );
    for arrow in [layout.up_arrow, layout.down_arrow] {
        let arrow = to_pixel_rect(arrow);
        fill_rect(
            frame,
            DESKTOP_PIXEL_WIDTH,
            DESKTOP_PIXEL_HEIGHT,
            arrow.left,
            arrow.top,
            arrow.right,
            arrow.bottom,
            [232, 232, 232, 255],
        );
        fill_rect(
            frame,
            DESKTOP_PIXEL_WIDTH,
            DESKTOP_PIXEL_HEIGHT,
            arrow.left,
            if arrow.top == bounds.top {
                arrow.bottom - FRAME_BORDER
            } else {
                arrow.top
            },
            arrow.right,
            if arrow.top == bounds.top {
                arrow.bottom
            } else {
                arrow.top + FRAME_BORDER
            },
            [59, 59, 59, 255],
        );
    }
    fill_rect(
        frame,
        DESKTOP_PIXEL_WIDTH,
        DESKTOP_PIXEL_HEIGHT,
        bounds.left,
        bounds.top,
        bounds.left + FRAME_BORDER,
        bounds.bottom,
        [59, 59, 59, 255],
    );
    let slider = to_pixel_rect(layout.slider);
    let handle = PixelRect {
        left: slider.left + 6,
        top: slider.top + 6,
        right: slider.right - 6,
        bottom: slider.bottom - 6,
    };
    fill_rect(
        frame,
        DESKTOP_PIXEL_WIDTH,
        DESKTOP_PIXEL_HEIGHT,
        handle.left,
        handle.top,
        handle.right,
        handle.bottom,
        [139, 139, 139, 255],
    );
    draw_scroll_arrow(frame, layout.up_arrow, true);
    draw_scroll_arrow(frame, layout.down_arrow, false);
}

fn draw_scroll_arrow(frame: &mut [u8], cell: DesktopRect, up: bool) {
    let rect = to_pixel_rect(cell);
    let center_x = (rect.left + rect.right) / 2;
    let center_y = (rect.top + rect.bottom) / 2;
    let ink = [20, 20, 20, 255];
    let (top, bottom) = if up {
        (center_y - 4, center_y + 4)
    } else {
        (center_y + 4, center_y - 4)
    };
    draw_control_line(
        frame,
        DESKTOP_PIXEL_WIDTH,
        DESKTOP_PIXEL_HEIGHT,
        (center_x - 8, bottom),
        (center_x, top),
        ink,
    );
    draw_control_line(
        frame,
        DESKTOP_PIXEL_WIDTH,
        DESKTOP_PIXEL_HEIGHT,
        (center_x, top),
        (center_x + 8, bottom),
        ink,
    );
}

fn draw_size_grip(frame: &mut [u8], cell: DesktopRect) {
    let rect = to_pixel_rect(cell);
    fill_rect(
        frame,
        DESKTOP_PIXEL_WIDTH,
        DESKTOP_PIXEL_HEIGHT,
        rect.left,
        rect.top,
        rect.left + FRAME_BORDER,
        rect.bottom,
        [59, 59, 59, 255],
    );
    let ink = [59, 59, 59, 255];
    for offset in [10, 18, 26] {
        draw_control_line(
            frame,
            DESKTOP_PIXEL_WIDTH,
            DESKTOP_PIXEL_HEIGHT,
            (rect.right - offset, rect.bottom - 4),
            (rect.right - 4, rect.bottom - offset),
            ink,
        );
    }
}

fn draw_sprite_at_scaled(frame: &mut [u8], sprite: &RiscOsSprite, x: i32, y: i32, scale: i32) {
    let scale = scale.max(1);
    for row in 0..sprite.height as i32 {
        for column in 0..sprite.width as i32 {
            let Some(color) = sprite.pixel(column as u32, row as u32) else {
                continue;
            };
            if color[3] != 0 {
                for dy in 0..scale {
                    for dx in 0..scale {
                        set_pixel(
                            frame,
                            DESKTOP_PIXEL_WIDTH,
                            DESKTOP_PIXEL_HEIGHT,
                            x + column * scale + dx,
                            y + row * scale + dy,
                            color,
                        );
                    }
                }
            }
        }
    }
}

fn draw_sprite_clipped_scaled(
    frame: &mut [u8],
    sprite: &RiscOsSprite,
    x: i32,
    y: i32,
    clip: PixelRect,
    inverted: bool,
    scale: i32,
) {
    let scale = scale.max(1);
    for row in 0..sprite.height as i32 {
        for column in 0..sprite.width as i32 {
            let Some(color) = sprite.pixel(column as u32, row as u32) else {
                continue;
            };
            if color[3] != 0 {
                let color = if inverted {
                    selected_sprite_colour(color)
                } else {
                    color
                };
                for dy in 0..scale {
                    for dx in 0..scale {
                        let px = x + column * scale + dx;
                        let py = y + row * scale + dy;
                        if px >= clip.left && px < clip.right && py >= clip.top && py < clip.bottom
                        {
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
        }
    }
}

// Neutral artwork reverses ink and paper. Coloured artwork retains its hue
// with a dark selected face, as in the desktop's classic palette sprites.
// Alpha is preserved so selecting an icon never paints its transparent margin.
pub(crate) fn selected_sprite_colour(mut color: [u8; 4]) -> [u8; 4] {
    let low = *color[..3].iter().min().unwrap();
    let high = *color[..3].iter().max().unwrap();
    if high - low < 24 {
        for component in &mut color[..3] {
            *component = 255 - *component;
        }
    } else {
        for component in &mut color[..3] {
            *component /= 2;
        }
    }
    color
}

fn draw_rgba_icon_fit(
    frame: &mut [u8],
    image: &DesktopIconImage,
    x: i32,
    y: i32,
    size: i32,
    clip: PixelRect,
    selected: bool,
) {
    // Average premultiplied samples when shrinking the Filer artwork so thin
    // outlines survive Small icons without colour halos at transparent edges.
    for row in 0..size {
        for column in 0..size {
            let px = x + column;
            let py = y + row;
            if px < clip.left || px >= clip.right || py < clip.top || py >= clip.bottom {
                continue;
            }
            let sx0 = column as u32 * image.width / size as u32;
            let sx1 = ((column + 1) as u32 * image.width / size as u32)
                .max(sx0 + 1)
                .min(image.width);
            let sy0 = row as u32 * image.height / size as u32;
            let sy1 = ((row + 1) as u32 * image.height / size as u32)
                .max(sy0 + 1)
                .min(image.height);
            let mut channels = [0u64; 3];
            let mut alpha = 0u64;
            let count = u64::from((sx1 - sx0) * (sy1 - sy0));
            for sy in sy0..sy1 {
                for sx in sx0..sx1 {
                    let mut color = image.rgba[(sy * image.width + sx) as usize];
                    if selected {
                        color = selected_sprite_colour(color);
                    }
                    alpha += u64::from(color[3]);
                    for c in 0..3 {
                        channels[c] += u64::from(color[c]) * u64::from(color[3]);
                    }
                }
            }
            if alpha > 0 && count > 0 {
                blend_pixel(
                    frame,
                    px,
                    py,
                    [
                        (channels[0] / alpha) as u8,
                        (channels[1] / alpha) as u8,
                        (channels[2] / alpha) as u8,
                        (alpha / count) as u8,
                    ],
                );
            }
        }
    }
}

fn draw_rgba_icon_image(
    frame: &mut [u8],
    image: &DesktopIconImage,
    x: i32,
    y: i32,
    clip: PixelRect,
    selected: bool,
) {
    for row in 0..image.height as i32 {
        for column in 0..image.width as i32 {
            let px = x + column;
            let py = y + row;
            if px < clip.left || px >= clip.right || py < clip.top || py >= clip.bottom {
                continue;
            }
            let Some(mut color) = image
                .rgba
                .get((row as u32 * image.width + column as u32) as usize)
                .copied()
            else {
                continue;
            };
            if selected {
                color = selected_sprite_colour(color);
            }
            blend_pixel(frame, px, py, color);
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

#[cfg(test)]
mod icon_colour_tests {
    use super::window_icon_colours;

    #[test]
    fn software_snapshot_path_rejects_modern_text_instead_of_blank_rendering() {
        let mut graphics = crate::graphics::GraphicsService::default();
        graphics
            .set_text_profile(
                crate::graphics::TextRenderingProfile::Modern,
                crate::graphics::TextEncoding::Utf8,
            )
            .unwrap();
        let mut frame = vec![0; 640 * 256 * 4];
        assert_eq!(
            super::render(graphics.snapshot(), &mut frame),
            Err(super::SoftwareRenderError::ModernTextRequiresVello)
        );
    }

    #[test]
    fn selected_filer_icon_keeps_transparent_margins_and_highlights_only_label() {
        use super::*;
        let background = [238, 238, 238, 255];
        let mut frame = background.repeat((DESKTOP_PIXEL_WIDTH * DESKTOP_PIXEL_HEIGHT) as usize);
        let icon = DesktopWindowIcon {
            handle: 1,
            owner_task_id: 1,
            window_handle: 1,
            label: "Images".into(),
            sprite_name: Some("directory".into()),
            high_resolution_image: Some(Arc::new(DesktopIconImage {
                width: 2,
                height: 1,
                rgba: vec![[30, 30, 30, 255], [248, 248, 248, 255]],
            })),
            bounds: DesktopRect {
                min_x: 0,
                min_y: DESKTOP_PIXEL_HEIGHT as i32 - 100,
                max_x: 180,
                max_y: DESKTOP_PIXEL_HEIGHT as i32,
            },
            flags: (1 << 21) | (1 << 3),
        };
        draw_window_icon(
            &mut frame,
            PixelRect {
                left: 0,
                top: 0,
                right: 180,
                bottom: 100,
            },
            &icon,
        );
        let pixel = |x: usize, y: usize| &frame[(y * DESKTOP_PIXEL_WIDTH as usize + x) * 4..][..4];
        assert_eq!(pixel(4, 4), background);
        assert_eq!(pixel(4, 90), background);
        assert_eq!(pixel(90, 96), [0, 0, 0, 255]);
        assert_eq!(pixel(89, 31), [225, 225, 225, 255]);
        assert_eq!(pixel(90, 31), [7, 7, 7, 255]);
    }

    #[test]
    fn explicit_menu_colours_invert_and_shade_without_hiding_text() {
        let flags = 7 << 24;
        assert_eq!(
            window_icon_colours(flags),
            ([0, 0, 0, 255], [255, 255, 255, 255])
        );
        assert_eq!(
            window_icon_colours(flags | (1 << 21)),
            ([255, 255, 255, 255], [0, 0, 0, 255])
        );
        let (ink, paper) = window_icon_colours(flags | (1 << 22));
        assert!(ink[0] > 0 && ink[0] < paper[0]);
        assert_eq!(paper, [255, 255, 255, 255]);
        assert_eq!(window_icon_colours(1 << 21).1, [65, 106, 228, 255]);
    }
}
