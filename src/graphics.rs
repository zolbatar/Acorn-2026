use crate::error::RuntimeError;
use std::sync::{Arc, Mutex};

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Point {
    pub x: i32,
    pub y: i32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GraphicsWindow {
    pub left: i32,
    pub bottom: i32,
    pub right: i32,
    pub top: i32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TextWindow {
    pub left: u16,
    pub top: u16,
    pub right: u16,
    pub bottom: u16,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ScreenMode {
    pub number: u8,
    pub logical_width: i32,
    pub logical_height: i32,
    pub pixel_width: u32,
    pub pixel_height: u32,
    pub text_columns: u16,
    pub text_rows: u16,
    pub colours: u32,
    pub bits_per_pixel: u8,
    pub graphics_enabled: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum GraphicsPrimitive {
    Line {
        from: Point,
        to: Point,
        plot_code: u8,
        action: u8,
        logical_colour: u32,
        clip: GraphicsWindow,
    },
    Point {
        at: Point,
        plot_code: u8,
        action: u8,
        logical_colour: u32,
        clip: GraphicsWindow,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GraphicsSnapshot {
    pub mode: ScreenMode,
    pub text_window: TextWindow,
    pub graphics_window: GraphicsWindow,
    pub graphics_origin: Point,
    pub graphics_cursor: Point,
    pub text_cursor: Point,
    pub text_colour: u8,
    pub graphics_action: u8,
    pub graphics_colour: u32,
    pub text_cells: Vec<u8>,
    pub primitives: Vec<GraphicsPrimitive>,
    pub raster_surface: Option<SharedRasterSurface>,
}

#[derive(Clone)]
pub struct SharedRasterSurface(Arc<Mutex<RasterData>>);

#[derive(Debug)]
struct RasterData {
    width: u32,
    height: u32,
    pixels: Vec<u8>,
}

impl std::fmt::Debug for SharedRasterSurface {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let data = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        formatter
            .debug_struct("SharedRasterSurface")
            .field("width", &data.width)
            .field("height", &data.height)
            .finish_non_exhaustive()
    }
}

impl PartialEq for SharedRasterSurface {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

impl Eq for SharedRasterSurface {}

impl SharedRasterSurface {
    fn new(width: u32, height: u32) -> Self {
        let mut pixels = vec![0; width as usize * height as usize * 4];
        for pixel in pixels.chunks_exact_mut(4) {
            pixel[3] = 0xFF;
        }
        Self(Arc::new(Mutex::new(RasterData {
            width,
            height,
            pixels,
        })))
    }

    fn set_pixel(&self, x: u32, y: u32, color: [u8; 4]) {
        let mut data = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if x >= data.width || y >= data.height {
            return;
        }
        let offset = (y as usize * data.width as usize + x as usize) * 4;
        data.pixels[offset..offset + 4].copy_from_slice(&color);
    }

    fn clear(&self) {
        let mut data = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for pixel in data.pixels.chunks_exact_mut(4) {
            pixel.copy_from_slice(&[0, 0, 0, 0xFF]);
        }
    }

    pub fn copy_to(&self, target: &mut [u8]) {
        let data = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let length = target.len().min(data.pixels.len());
        target[..length].copy_from_slice(&data.pixels[..length]);
    }
}

struct PendingVdu {
    command: u8,
    parameters: Vec<u8>,
    expected: usize,
}

/// Stateful VDU stream parser and a host-independent BASIC graphics scene.
///
/// The service preserves logical graphics coordinates and clipping state; a
/// windowing backend can render the resulting snapshot without changing BASIC
/// or SWI behavior.
pub struct GraphicsService {
    snapshot: GraphicsSnapshot,
    pending_vdu: Option<PendingVdu>,
}

impl Default for GraphicsService {
    fn default() -> Self {
        let mode = screen_mode(0).expect("mode zero is part of the hosted profile");
        let text_window = default_text_window(mode);
        let graphics_window = default_graphics_window(mode);
        Self {
            snapshot: GraphicsSnapshot {
                mode,
                text_window,
                graphics_window,
                graphics_origin: Point::default(),
                graphics_cursor: Point::default(),
                text_cursor: Point::default(),
                text_colour: default_foreground_colour(mode),
                graphics_action: 0,
                graphics_colour: 7,
                text_cells: vec![
                    b' ';
                    usize::from(mode.text_columns) * usize::from(mode.text_rows)
                ],
                primitives: Vec::new(),
                raster_surface: None,
            },
            pending_vdu: None,
        }
    }
}

impl GraphicsService {
    /// Submit one byte from OS_WriteC. `Some(byte)` is forwarded to the host
    /// console; VDU control bytes and their parameters are consumed here.
    pub fn write_byte(&mut self, byte: u8) -> Result<Option<u8>, RuntimeError> {
        if let Some(pending) = &mut self.pending_vdu {
            pending.parameters.push(byte);
            if pending.parameters.len() == pending.expected {
                let complete = self.pending_vdu.take().expect("pending VDU command exists");
                return self.apply_vdu(complete.command, &complete.parameters);
            }
            return Ok(None);
        }

        if byte == 0x7F {
            self.move_text_cursor(8);
            return Ok(Some(byte));
        }
        if byte < 0x20 {
            let expected = vdu_parameter_count(byte);
            if expected != 0 {
                self.pending_vdu = Some(PendingVdu {
                    command: byte,
                    parameters: Vec::with_capacity(expected),
                    expected,
                });
                return Ok(None);
            }
            return self.apply_vdu(byte, &[]);
        }

        self.write_text_cell(byte);
        Ok(Some(byte))
    }

    /// Apply one OS_Plot operation using RISC OS logical coordinates.
    pub fn plot(&mut self, plot_code: u8, x: i32, y: i32) -> Result<(), RuntimeError> {
        if !self.snapshot.mode.graphics_enabled {
            return Ok(());
        }
        let group = plot_code & 0xF8;
        let subcode = plot_code & 0x07;
        let relative = subcode < 4;
        let operation = subcode & 0x03;
        let target = if relative {
            Point {
                x: self.snapshot.graphics_cursor.x.saturating_add(x),
                y: self.snapshot.graphics_cursor.y.saturating_add(y),
            }
        } else {
            Point {
                x: x.saturating_add(self.snapshot.graphics_origin.x),
                y: y.saturating_add(self.snapshot.graphics_origin.y),
            }
        };

        match group {
            0 | 8 | 16 | 24 | 32 | 40 | 48 | 56 => {
                if operation == 0 {
                    self.snapshot.graphics_cursor = target;
                } else {
                    let from = self.snapshot.graphics_cursor;
                    let primitive = GraphicsPrimitive::Line {
                        from,
                        to: target,
                        plot_code,
                        action: self.snapshot.graphics_action,
                        logical_colour: self.snapshot.graphics_colour,
                        clip: self.snapshot.graphics_window,
                    };
                    self.record_primitive(primitive);
                    self.snapshot.graphics_cursor = target;
                }
            }
            64 => {
                if operation == 0 {
                    self.snapshot.graphics_cursor = target;
                } else {
                    let primitive = GraphicsPrimitive::Point {
                        at: target,
                        plot_code,
                        action: self.snapshot.graphics_action,
                        logical_colour: self.snapshot.graphics_colour,
                        clip: self.snapshot.graphics_window,
                    };
                    self.record_primitive(primitive);
                    self.snapshot.graphics_cursor = target;
                }
            }
            _ => {
                return Err(RuntimeError::Program(format!(
                    "PLOT code &{plot_code:02X} is not supported by the hosted graphics profile"
                )));
            }
        }
        Ok(())
    }

    pub fn set_rgb_gcol(&mut self, palette_entry: u32) {
        self.snapshot.graphics_colour = palette_entry;
    }

    pub fn set_extended_mode(
        &mut self,
        pixel_width: u32,
        pixel_height: u32,
        x_eigenfactor: u8,
        y_eigenfactor: u8,
    ) -> Result<(), RuntimeError> {
        let logical_width = pixel_width
            .checked_mul(1_u32 << x_eigenfactor)
            .and_then(|width| i32::try_from(width).ok())
            .ok_or_else(|| RuntimeError::Program("extended mode width is too large".into()))?;
        let logical_height = pixel_height
            .checked_mul(1_u32 << y_eigenfactor)
            .and_then(|height| i32::try_from(height).ok())
            .ok_or_else(|| RuntimeError::Program("extended mode height is too large".into()))?;
        let mode = ScreenMode {
            number: u8::MAX,
            logical_width,
            logical_height,
            pixel_width,
            pixel_height,
            text_columns: 80,
            text_rows: 25,
            colours: 16_777_216,
            bits_per_pixel: 32,
            graphics_enabled: true,
        };
        self.snapshot.mode = mode;
        self.snapshot.text_window = default_text_window(mode);
        self.snapshot.graphics_window = default_graphics_window(mode);
        self.snapshot.graphics_origin = Point::default();
        self.snapshot.graphics_cursor = Point::default();
        self.snapshot.text_cursor = Point::default();
        self.snapshot.text_cells =
            vec![b' '; usize::from(mode.text_columns) * usize::from(mode.text_rows)];
        self.snapshot.primitives.clear();
        self.snapshot.raster_surface = Some(SharedRasterSurface::new(pixel_width, pixel_height));
        self.snapshot.text_colour = default_foreground_colour(mode);
        self.snapshot.graphics_action = 0;
        self.snapshot.graphics_colour = u32::from(default_foreground_colour(mode));
        Ok(())
    }

    fn record_primitive(&mut self, primitive: GraphicsPrimitive) {
        if let Some(surface) = &self.snapshot.raster_surface {
            rasterize_primitive(surface, &self.snapshot, &primitive);
        } else {
            self.snapshot.primitives.push(primitive);
        }
    }

    pub fn snapshot(&self) -> &GraphicsSnapshot {
        &self.snapshot
    }

    /// Replace the visible scene with a snapshot produced by the runtime.
    pub fn replace_snapshot(&mut self, snapshot: GraphicsSnapshot) {
        self.snapshot = snapshot;
        self.pending_vdu = None;
    }

    fn apply_vdu(&mut self, command: u8, parameters: &[u8]) -> Result<Option<u8>, RuntimeError> {
        match command {
            1 => return Ok(None),
            7 => return Ok(Some(7)),
            8 => self.move_text_cursor(8),
            9 => self.move_text_cursor(9),
            10 => self.line_feed(),
            11 => self.move_text_cursor(11),
            12 => {
                self.clear_text_window();
                self.snapshot.text_cursor = Point::default();
            }
            13 => {
                self.snapshot.text_cursor.x = 0;
                return Ok(Some(command));
            }
            16 => {
                self.snapshot.primitives.clear();
                if let Some(surface) = &self.snapshot.raster_surface {
                    surface.clear();
                }
            }
            17 => self.snapshot.text_colour = parameters[0],
            18 => {
                self.snapshot.graphics_action = parameters[0];
                self.snapshot.graphics_colour = u32::from(parameters[1]);
            }
            20 => {
                self.snapshot.text_colour = default_foreground_colour(self.snapshot.mode);
                self.snapshot.graphics_action = 0;
                self.snapshot.graphics_colour =
                    u32::from(default_foreground_colour(self.snapshot.mode));
            }
            22 => self.set_mode(parameters[0])?,
            24 => self.set_graphics_window(parameters),
            25 => {
                let x = i32::from(i16::from_le_bytes([parameters[1], parameters[2]]));
                let y = i32::from(i16::from_le_bytes([parameters[3], parameters[4]]));
                self.plot(parameters[0], x, y)?;
            }
            26 => self.reset_windows(),
            28 => self.set_text_window(parameters),
            29 => {
                self.snapshot.graphics_origin = Point {
                    x: i32::from(i16::from_le_bytes([parameters[0], parameters[1]])),
                    y: i32::from(i16::from_le_bytes([parameters[2], parameters[3]])),
                };
            }
            30 => self.snapshot.text_cursor = Point::default(),
            31 => {
                self.snapshot.text_cursor = Point {
                    x: i32::from(parameters[0]),
                    y: i32::from(parameters[1]),
                };
                self.clamp_text_cursor();
            }
            _ => {}
        }

        match command {
            8..=11 => Ok(Some(command)),
            _ => Ok(None),
        }
    }

    fn set_mode(&mut self, number: u8) -> Result<(), RuntimeError> {
        let mode = screen_mode(number).ok_or_else(|| {
            RuntimeError::Program(format!(
                "screen mode {number} is not supported by the hosted graphics profile"
            ))
        })?;
        self.snapshot.mode = mode;
        self.snapshot.text_window = default_text_window(mode);
        self.snapshot.graphics_window = default_graphics_window(mode);
        self.snapshot.graphics_origin = Point::default();
        self.snapshot.graphics_cursor = Point::default();
        self.snapshot.text_cursor = Point::default();
        self.snapshot.text_cells =
            vec![b' '; usize::from(mode.text_columns) * usize::from(mode.text_rows)];
        self.snapshot.primitives.clear();
        self.snapshot.raster_surface = None;
        self.snapshot.text_colour = default_foreground_colour(mode);
        self.snapshot.graphics_action = 0;
        self.snapshot.graphics_colour = u32::from(default_foreground_colour(mode));
        Ok(())
    }

    fn set_graphics_window(&mut self, parameters: &[u8]) {
        let x1 = i32::from(i16::from_le_bytes([parameters[0], parameters[1]]));
        let y1 = i32::from(i16::from_le_bytes([parameters[2], parameters[3]]));
        let x2 = i32::from(i16::from_le_bytes([parameters[4], parameters[5]]));
        let y2 = i32::from(i16::from_le_bytes([parameters[6], parameters[7]]));
        self.snapshot.graphics_window = GraphicsWindow {
            left: x1.min(x2),
            bottom: y1.min(y2),
            right: x1.max(x2),
            top: y1.max(y2),
        };
    }

    fn set_text_window(&mut self, parameters: &[u8]) {
        let x1 = u16::from(parameters[0]).min(self.snapshot.mode.text_columns - 1);
        let y1 = u16::from(parameters[1]).min(self.snapshot.mode.text_rows - 1);
        let x2 = u16::from(parameters[2]).min(self.snapshot.mode.text_columns - 1);
        let y2 = u16::from(parameters[3]).min(self.snapshot.mode.text_rows - 1);
        self.snapshot.text_window = TextWindow {
            left: x1.min(x2),
            top: y1.min(y2),
            right: x1.max(x2),
            bottom: y1.max(y2),
        };
        self.snapshot.text_cursor = Point::default();
    }

    fn reset_windows(&mut self) {
        self.snapshot.text_window = default_text_window(self.snapshot.mode);
        self.snapshot.graphics_window = default_graphics_window(self.snapshot.mode);
        self.snapshot.text_cursor = Point::default();
    }

    fn write_text_cell(&mut self, byte: u8) {
        let window_width =
            i32::from(self.snapshot.text_window.right - self.snapshot.text_window.left + 1);
        let window_height =
            i32::from(self.snapshot.text_window.bottom - self.snapshot.text_window.top + 1);
        let x = self.snapshot.text_window.left + self.snapshot.text_cursor.x as u16;
        let y = self.snapshot.text_window.top + self.snapshot.text_cursor.y as u16;
        if x < self.snapshot.mode.text_columns && y < self.snapshot.mode.text_rows {
            let index =
                usize::from(y) * usize::from(self.snapshot.mode.text_columns) + usize::from(x);
            self.snapshot.text_cells[index] = byte;
        }
        self.snapshot.text_cursor.x += 1;
        if self.snapshot.text_cursor.x >= window_width {
            self.snapshot.text_cursor.x = 0;
            self.snapshot.text_cursor.y += 1;
            if self.snapshot.text_cursor.y >= window_height {
                self.scroll_text_window();
                self.snapshot.text_cursor.y = window_height - 1;
            }
        }
    }

    fn move_text_cursor(&mut self, control: u8) {
        match control {
            8 => self.snapshot.text_cursor.x = (self.snapshot.text_cursor.x - 1).max(0),
            9 => self.snapshot.text_cursor.x += 1,
            11 => self.snapshot.text_cursor.y = (self.snapshot.text_cursor.y - 1).max(0),
            _ => {}
        }
        self.clamp_text_cursor();
    }

    fn line_feed(&mut self) {
        let height =
            i32::from(self.snapshot.text_window.bottom - self.snapshot.text_window.top + 1);
        if self.snapshot.text_cursor.y + 1 >= height {
            self.scroll_text_window();
            self.snapshot.text_cursor.y = height - 1;
        } else {
            self.snapshot.text_cursor.y += 1;
        }
    }

    fn clamp_text_cursor(&mut self) {
        let width = i32::from(self.snapshot.text_window.right - self.snapshot.text_window.left);
        let height = i32::from(self.snapshot.text_window.bottom - self.snapshot.text_window.top);
        self.snapshot.text_cursor.x = self.snapshot.text_cursor.x.clamp(0, width);
        self.snapshot.text_cursor.y = self.snapshot.text_cursor.y.clamp(0, height);
    }

    fn clear_text_window(&mut self) {
        for y in self.snapshot.text_window.top..=self.snapshot.text_window.bottom {
            for x in self.snapshot.text_window.left..=self.snapshot.text_window.right {
                let index =
                    usize::from(y) * usize::from(self.snapshot.mode.text_columns) + usize::from(x);
                self.snapshot.text_cells[index] = b' ';
            }
        }
    }

    fn scroll_text_window(&mut self) {
        let columns = usize::from(self.snapshot.mode.text_columns);
        let left = usize::from(self.snapshot.text_window.left);
        let right = usize::from(self.snapshot.text_window.right);
        let top = usize::from(self.snapshot.text_window.top);
        let bottom = usize::from(self.snapshot.text_window.bottom);
        for row in top..bottom {
            for column in left..=right {
                let source = (row + 1) * columns + column;
                let destination = row * columns + column;
                self.snapshot.text_cells[destination] = self.snapshot.text_cells[source];
            }
        }
        let last_row = bottom * columns;
        for column in left..=right {
            self.snapshot.text_cells[last_row + column] = b' ';
        }
    }
}

fn vdu_parameter_count(command: u8) -> usize {
    match command {
        1 | 17 | 22 => 1,
        18 => 2,
        19 => 5,
        23 => 9,
        24 => 8,
        25 => 5,
        28 | 29 => 4,
        31 => 2,
        _ => 0,
    }
}

fn screen_mode(number: u8) -> Option<ScreenMode> {
    // RISC OS PRM, Volume 4, Table B. Mode 32 is unassigned. The modes in
    // the range 128..=164 are the BBC BASIC shadow-memory aliases for 0..=36;
    // shadow memory has no separate visual representation in this hosted
    // profile, so those aliases share the base mode's display description.
    let base_number = if (128..=164).contains(&number) {
        number - 128
    } else {
        number
    };
    let (
        logical_width,
        logical_height,
        pixel_width,
        pixel_height,
        text_columns,
        text_rows,
        colours,
        bits_per_pixel,
        graphics_enabled,
    ) = match base_number {
        0 => (1280, 1024, 640, 256, 80, 32, 2, 1, true),
        1 => (1280, 1024, 320, 256, 40, 32, 4, 2, true),
        2 => (1280, 1024, 160, 256, 20, 32, 16, 4, true),
        // Text-only modes use a host raster sized to the character grid; the
        // guest has no graphics coordinate space in these modes.
        3 => (0, 0, 640, 250, 80, 25, 2, 1, false),
        4 => (1280, 1024, 320, 256, 40, 32, 2, 1, true),
        5 => (1280, 1024, 160, 256, 20, 32, 4, 2, true),
        6 => (0, 0, 640, 250, 40, 25, 2, 1, false),
        // Mode 7 uses the SAA5050-style Teletext character display.
        7 => (0, 0, 480, 500, 40, 25, 16, 0, false),
        8 => (1280, 1024, 640, 256, 80, 32, 4, 2, true),
        9 => (1280, 1024, 320, 256, 40, 32, 16, 4, true),
        10 => (1280, 1024, 160, 256, 20, 32, 256, 8, true),
        11 => (1280, 1000, 640, 250, 80, 25, 4, 2, true),
        12 => (1280, 1024, 640, 256, 80, 32, 16, 4, true),
        13 => (1280, 1024, 320, 256, 40, 32, 256, 8, true),
        14 => (1280, 1000, 640, 250, 80, 25, 16, 4, true),
        15 => (1280, 1024, 640, 256, 80, 32, 256, 8, true),
        16 => (2112, 1024, 1056, 256, 132, 32, 16, 4, true),
        17 => (2112, 1000, 1056, 250, 132, 25, 16, 4, true),
        18 => (1280, 1024, 640, 512, 80, 64, 2, 1, true),
        19 => (1280, 1024, 640, 512, 80, 64, 4, 2, true),
        20 => (1280, 1024, 640, 512, 80, 64, 16, 4, true),
        21 => (1280, 1024, 640, 512, 80, 64, 256, 8, true),
        22 => (768, 576, 768, 288, 96, 36, 16, 4, true),
        23 => (2304, 1792, 1152, 896, 144, 56, 2, 1, true),
        24 => (2112, 1024, 1056, 256, 132, 32, 256, 8, true),
        25 => (1280, 960, 640, 480, 80, 60, 2, 1, true),
        26 => (1280, 960, 640, 480, 80, 60, 4, 2, true),
        27 => (1280, 960, 640, 480, 80, 60, 16, 4, true),
        28 => (1280, 960, 640, 480, 80, 60, 256, 8, true),
        29 => (1600, 1200, 800, 600, 100, 75, 2, 1, true),
        30 => (1600, 1200, 800, 600, 100, 75, 4, 2, true),
        31 => (1600, 1200, 800, 600, 100, 75, 16, 4, true),
        33 => (1536, 1152, 768, 288, 96, 36, 2, 1, true),
        34 => (1536, 1152, 768, 288, 96, 36, 4, 2, true),
        35 => (1536, 1152, 768, 288, 96, 36, 16, 4, true),
        36 => (1536, 1152, 768, 288, 96, 36, 256, 8, true),
        37 => (1792, 1408, 896, 352, 112, 44, 2, 1, true),
        38 => (1792, 1408, 896, 352, 112, 44, 4, 2, true),
        39 => (1792, 1408, 896, 352, 112, 44, 16, 4, true),
        40 => (1792, 1408, 896, 352, 112, 44, 256, 8, true),
        41 => (1280, 1408, 640, 352, 80, 44, 2, 1, true),
        42 => (1280, 1408, 640, 352, 80, 44, 4, 2, true),
        43 => (1280, 1408, 640, 352, 80, 44, 16, 4, true),
        44 => (1280, 800, 640, 200, 80, 25, 2, 1, true),
        45 => (1280, 800, 640, 200, 80, 25, 4, 2, true),
        46 => (1280, 800, 640, 200, 80, 25, 16, 4, true),
        _ => return None,
    };
    Some(ScreenMode {
        number,
        logical_width,
        logical_height,
        pixel_width,
        pixel_height,
        text_columns,
        text_rows,
        colours,
        bits_per_pixel,
        graphics_enabled,
    })
}

fn rasterize_primitive(
    surface: &SharedRasterSurface,
    snapshot: &GraphicsSnapshot,
    primitive: &GraphicsPrimitive,
) {
    match primitive {
        GraphicsPrimitive::Point {
            at,
            clip,
            logical_colour,
            ..
        } => {
            if point_inside(*at, *clip, snapshot) {
                let (x, y) = screen_point(*at, snapshot);
                surface.set_pixel(x, y, graphics_colour(*logical_colour, snapshot.mode));
            }
        }
        GraphicsPrimitive::Line {
            from,
            to,
            clip,
            logical_colour,
            ..
        } => {
            let Some((from, to)) = clip_line(*from, *to, *clip, snapshot) else {
                return;
            };
            let (x0_u, y0_u) = screen_point(from, snapshot);
            let (x1_u, y1_u) = screen_point(to, snapshot);
            let (mut x0, mut y0) = (x0_u as i32, y0_u as i32);
            let (x1, y1) = (x1_u as i32, y1_u as i32);
            let dx = (x1 as i64 - x0 as i64).abs() as i32;
            let sx = if x0 < x1 { 1 } else { -1 };
            let dy = -((y1 as i64 - y0 as i64).abs() as i32);
            let sy = if y0 < y1 { 1 } else { -1 };
            let mut error = dx + dy;
            let color = graphics_colour(*logical_colour, snapshot.mode);
            loop {
                surface.set_pixel(x0 as u32, y0 as u32, color);
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
    }
}

fn point_inside(point: Point, clip: GraphicsWindow, snapshot: &GraphicsSnapshot) -> bool {
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

fn screen_point(point: Point, snapshot: &GraphicsSnapshot) -> (u32, u32) {
    let width = i64::from(snapshot.mode.pixel_width);
    let height = i64::from(snapshot.mode.pixel_height);
    let x = i64::from(point.x) * width / i64::from(snapshot.mode.logical_width);
    let y = i64::from(snapshot.mode.logical_height - 1 - point.y) * height
        / i64::from(snapshot.mode.logical_height);
    (x.clamp(0, width - 1) as u32, y.clamp(0, height - 1) as u32)
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
    let (left, right) = (
        clip.left.max(0) as f64,
        clip.right.min(snapshot.mode.logical_width - 1) as f64,
    );
    let (bottom, top) = (
        clip.bottom.max(0) as f64,
        clip.top.min(snapshot.mode.logical_height - 1) as f64,
    );
    if left > right || bottom > top {
        return None;
    }
    let (mut x0, mut y0, mut x1, mut y1) = (from.x as f64, from.y as f64, to.x as f64, to.y as f64);
    loop {
        let c0 = line_out_code(x0, y0, left, right, bottom, top);
        let c1 = line_out_code(x1, y1, left, right, bottom, top);
        if c0 | c1 == 0 {
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
        if c0 & c1 != 0 {
            return None;
        }
        let outside = if c0 != 0 { c0 } else { c1 };
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
        if outside == c0 {
            x0 = x;
            y0 = y;
        } else {
            x1 = x;
            y1 = y;
        }
    }
}

fn line_out_code(x: f64, y: f64, left: f64, right: f64, bottom: f64, top: f64) -> u8 {
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

pub(crate) fn graphics_colour(value: u32, mode: ScreenMode) -> [u8; 4] {
    if value <= 0xFF {
        const PALETTE: [[u8; 4]; 8] = [
            [0x00, 0x00, 0x00, 0xFF],
            [0xFF, 0x00, 0x00, 0xFF],
            [0x00, 0xFF, 0x00, 0xFF],
            [0xFF, 0xFF, 0x00, 0xFF],
            [0x00, 0x00, 0xFF, 0xFF],
            [0xFF, 0x00, 0xFF, 0xFF],
            [0x00, 0xFF, 0xFF, 0xFF],
            [0xFF, 0xFF, 0xFF, 0xFF],
        ];
        match mode.bits_per_pixel {
            8 => {
                // The BASIC GCOL byte stores two bits per RGB component and
                // two tint bits. Without an explicit TINT command, the
                // foreground uses the default full tint and the background
                // uses no tint.
                let colour = value as u8;
                let tint = if colour & 0x80 == 0 { 3 } else { 0 };
                let channel =
                    |shift: u32| ((((u32::from(colour) >> shift) & 3) * 4 + tint) * 17) as u8;
                [channel(0), channel(2), channel(4), 0xFF]
            }
            1 | 2 | 4 => {
                let logical = (value % mode.colours.max(1)) as usize;
                let physical = match mode.bits_per_pixel {
                    1 => {
                        if logical == 0 {
                            0
                        } else {
                            7
                        }
                    }
                    2 => [0, 1, 3, 7][logical.min(3)],
                    _ => logical.min(15) as u8,
                };
                PALETTE[usize::from(physical & 7)]
            }
            _ => PALETTE[(value as usize) & 7],
        }
    } else {
        [
            ((value >> 8) & 0xFF) as u8,
            ((value >> 16) & 0xFF) as u8,
            (value >> 24) as u8,
            0xFF,
        ]
    }
}

fn default_text_window(mode: ScreenMode) -> TextWindow {
    TextWindow {
        left: 0,
        top: 0,
        right: mode.text_columns - 1,
        bottom: mode.text_rows - 1,
    }
}

fn default_graphics_window(mode: ScreenMode) -> GraphicsWindow {
    if !mode.graphics_enabled {
        return GraphicsWindow {
            left: 0,
            bottom: 0,
            right: 0,
            top: 0,
        };
    }
    GraphicsWindow {
        left: 0,
        bottom: 0,
        right: mode.logical_width - 1,
        top: mode.logical_height - 1,
    }
}

fn default_foreground_colour(mode: ScreenMode) -> u8 {
    if mode.bits_per_pixel == 8 { 63 } else { 7 }
}
