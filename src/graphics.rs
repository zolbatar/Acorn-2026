use crate::error::RuntimeError;
use std::sync::{Arc, Mutex};

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum GraphicsProfile {
    #[default]
    Hosted,
    Agon,
}

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
    pub profile: GraphicsProfile,
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
    ClearRectangle {
        bounds: GraphicsWindow,
        logical_colour: u32,
    },
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
    /// Monotonically advances when guest-visible display state changes. Used
    /// by the compositor to avoid rebuilding an unchanged raster image.
    pub revision: u64,
    pub mode: ScreenMode,
    pub text_window: TextWindow,
    pub graphics_window: GraphicsWindow,
    /// Temporary Wimp rectangle clip active while painting a redraw/update.
    /// This remains caller-visible only inside the hosted renderer adapter.
    pub wimp_clip: Option<GraphicsWindow>,
    /// Logical palette metadata is retained separately from the RGBA backing.
    /// Guest palette mutation and indexed storage are still compatibility gaps.
    pub logical_palette: Vec<[u8; 4]>,
    /// Tracks whether guest graphics drawing should replace the modern
    /// text-only paper base with the classic raster.
    pub graphics_content_present: bool,
    pub modern_text_background: bool,
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

    fn read_pixel(&self, x: u32, y: u32) -> Option<[u8; 4]> {
        let data = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if x >= data.width || y >= data.height {
            return None;
        }
        let offset = (y as usize * data.width as usize + x as usize) * 4;
        Some(data.pixels[offset..offset + 4].try_into().ok()?)
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

    fn fill_rect(&self, bounds: (u32, u32, u32, u32), color: [u8; 4]) {
        let mut data = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let (left, top, right, bottom) = bounds;
        for y in top.min(data.height)..bottom.min(data.height) {
            for x in left.min(data.width)..right.min(data.width) {
                let offset = (y as usize * data.width as usize + x as usize) * 4;
                data.pixels[offset..offset + 4].copy_from_slice(&color);
            }
        }
    }
}

#[derive(Clone)]
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
    profile: GraphicsProfile,
}

impl Default for GraphicsService {
    fn default() -> Self {
        let mode = screen_mode(0).expect("mode zero is part of the hosted profile");
        let text_window = default_text_window(mode);
        let graphics_window = default_graphics_window(mode);
        Self {
            snapshot: GraphicsSnapshot {
                revision: 0,
                mode,
                text_window,
                graphics_window,
                wimp_clip: None,
                logical_palette: default_palette(mode),
                graphics_content_present: false,
                modern_text_background: !is_teletext_mode(mode.number),
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
                raster_surface: Some(SharedRasterSurface::new(
                    mode.pixel_width,
                    mode.pixel_height,
                )),
            },
            pending_vdu: None,
            profile: GraphicsProfile::Hosted,
        }
    }
}

impl GraphicsService {
    /// Resume a graphics scene from a published task snapshot. The desktop's
    /// headless renderer uses this same stateful path as the live window.
    pub(crate) fn from_snapshot(snapshot: GraphicsSnapshot) -> Self {
        Self {
            profile: snapshot.mode.profile,
            snapshot,
            pending_vdu: None,
        }
    }

    /// Copy the current mode and drawing state into an independent output
    /// destination, including a separate authoritative true-colour raster.
    pub(crate) fn new_window_output(&self) -> Self {
        let mut snapshot = self.snapshot.clone();
        snapshot.text_cells.fill(b' ');
        snapshot.text_cursor = Point::default();
        snapshot.graphics_cursor = Point::default();
        snapshot.primitives.clear();
        snapshot.raster_surface = self.snapshot.raster_surface.as_ref().map(|_| {
            SharedRasterSurface::new(snapshot.mode.pixel_width, snapshot.mode.pixel_height)
        });
        snapshot.wimp_clip = None;
        snapshot.graphics_content_present = false;
        snapshot.revision = snapshot.revision.wrapping_add(1);
        Self {
            snapshot,
            pending_vdu: None,
            profile: self.profile,
        }
    }

    pub(crate) fn mode_pixel_count(&self) -> u64 {
        u64::from(self.snapshot.mode.pixel_width) * u64::from(self.snapshot.mode.pixel_height)
    }

    /// Read a pixel from the active CPU raster and return the closest
    /// representable guest colour/tint pair. Indexed RGBA surfaces cannot
    /// preserve every logical index or plot action, so those reads are
    /// necessarily approximate until indexed backing storage is added.
    pub(crate) fn read_point(&self, x: i32, y: i32) -> Option<(u32, u32)> {
        let mode = self.snapshot.mode;
        if !mode.graphics_enabled {
            return None;
        }
        let point = Point {
            x: x.saturating_add(self.snapshot.graphics_origin.x),
            y: y.saturating_add(self.snapshot.graphics_origin.y),
        };
        if point.x < 0
            || point.y < 0
            || point.x >= mode.logical_width
            || point.y >= mode.logical_height
        {
            return None;
        }
        let surface = self.snapshot.raster_surface.as_ref()?;
        let (pixel_x, pixel_y) = screen_point(point, &self.snapshot);
        let pixel = surface.read_pixel(pixel_x, pixel_y)?;

        if mode.bits_per_pixel == 32 {
            // The hosted C16M path stores colours in the byte order accepted
            // by ColourTrans_SetGCOL: BB GG RR 00.
            let colour = (u32::from(pixel[2]) << 24)
                | (u32::from(pixel[1]) << 16)
                | (u32::from(pixel[0]) << 8);
            return Some((colour, 0));
        }

        let nearest = self
            .snapshot
            .logical_palette
            .iter()
            .enumerate()
            .min_by_key(|(_, candidate)| {
                let channel_distance = |actual: u8, expected: u8| {
                    let difference = i32::from(actual) - i32::from(expected);
                    difference * difference
                };
                channel_distance(pixel[0], candidate[0])
                    + channel_distance(pixel[1], candidate[1])
                    + channel_distance(pixel[2], candidate[2])
            })
            .map(|(index, _)| index as u32)?;
        if mode.bits_per_pixel == 8 {
            let tint = if nearest & 0x80 == 0 { 255 } else { 0 };
            Some((nearest & 0x3F, tint))
        } else {
            Some((nearest, 0))
        }
    }

    pub(crate) fn mode_after_vdu_byte(&self, byte: u8) -> Option<ScreenMode> {
        let pending = self.pending_vdu.as_ref()?;
        if pending.command == 22 && pending.parameters.is_empty() {
            screen_mode_for_profile(byte, self.profile)
        } else {
            None
        }
    }

    pub(crate) fn set_wimp_redraw_clip(&mut self, clip: Option<GraphicsWindow>) {
        if self.snapshot.wimp_clip != clip {
            self.snapshot.wimp_clip = clip;
            self.snapshot.revision = self.snapshot.revision.wrapping_add(1);
        }
    }

    pub(crate) fn note_external_plot(&mut self, draws_pixels: bool) {
        if draws_pixels {
            self.snapshot.graphics_content_present = true;
        }
        self.snapshot.revision = self.snapshot.revision.wrapping_add(1);
    }

    /// Submit one byte from OS_WriteC. `Some(byte)` is forwarded to the host
    /// console; VDU control bytes and their parameters are consumed here.
    pub fn write_byte(&mut self, byte: u8) -> Result<Option<u8>, RuntimeError> {
        self.snapshot.revision = self.snapshot.revision.wrapping_add(1);
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
        self.snapshot.revision = self.snapshot.revision.wrapping_add(1);
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
        let clip = self.effective_graphics_clip().unwrap_or(GraphicsWindow {
            left: 1,
            bottom: 1,
            right: 0,
            top: 0,
        });

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
                        clip,
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
                        clip,
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
        self.snapshot.revision = self.snapshot.revision.wrapping_add(1);
    }

    pub(crate) fn clear_wimp_region(&mut self, bounds: GraphicsWindow, logical_colour: u32) {
        self.clear_graphics_region(bounds, logical_colour, false);
        self.clear_text_cells_in_graphics_region(bounds);
        self.snapshot.revision = self.snapshot.revision.wrapping_add(1);
    }

    fn clear_graphics_region(
        &mut self,
        bounds: GraphicsWindow,
        logical_colour: u32,
        guest_graphics_operation: bool,
    ) {
        let primitive = GraphicsPrimitive::ClearRectangle {
            bounds,
            logical_colour,
        };
        if let Some(surface) = &self.snapshot.raster_surface {
            rasterize_primitive(surface, &self.snapshot, &primitive);
        } else {
            self.snapshot.primitives.push(primitive);
        }
        if guest_graphics_operation {
            self.snapshot.graphics_content_present = true;
        }
        self.snapshot.revision = self.snapshot.revision.wrapping_add(1);
    }

    pub fn set_extended_mode(
        &mut self,
        pixel_width: u32,
        pixel_height: u32,
        x_eigenfactor: u8,
        y_eigenfactor: u8,
    ) -> Result<(), RuntimeError> {
        self.snapshot.revision = self.snapshot.revision.wrapping_add(1);
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
            profile: self.profile,
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
        self.snapshot.logical_palette = Vec::new();
        self.snapshot.graphics_content_present = false;
        self.snapshot.modern_text_background = false;
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
        self.snapshot.graphics_content_present = true;
    }

    pub fn snapshot(&self) -> &GraphicsSnapshot {
        &self.snapshot
    }

    pub fn set_profile(&mut self, profile: GraphicsProfile) -> Result<(), RuntimeError> {
        if self.profile == profile {
            return Ok(());
        }
        self.profile = profile;
        self.snapshot.revision = self.snapshot.revision.wrapping_add(1);
        self.pending_vdu = None;
        // A different target can assign a different meaning to the current
        // mode number (or not support it at all). Start that target from its
        // own default mode instead of carrying screen state across runtimes.
        self.set_mode(0)
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
            16 => self.clear_graphics(),
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
        let mode = screen_mode_for_profile(number, self.profile).ok_or_else(|| {
            RuntimeError::Program(format!(
                "screen mode {number} is not supported by the {:?} graphics profile",
                self.profile
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
        self.snapshot.raster_surface = Some(SharedRasterSurface::new(
            mode.pixel_width,
            mode.pixel_height,
        ));
        self.snapshot.logical_palette = default_palette(mode);
        self.snapshot.graphics_content_present = false;
        self.snapshot.modern_text_background = !is_teletext_mode(mode.number);
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
        if x < self.snapshot.mode.text_columns
            && y < self.snapshot.mode.text_rows
            && self.text_cell_intersects_wimp_clip(x, y)
        {
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
                if !self.text_cell_intersects_wimp_clip(x, y) {
                    continue;
                }
                let index =
                    usize::from(y) * usize::from(self.snapshot.mode.text_columns) + usize::from(x);
                self.snapshot.text_cells[index] = b' ';
            }
        }
    }

    fn effective_graphics_clip(&self) -> Option<GraphicsWindow> {
        let clip = self.snapshot.graphics_window;
        let Some(wimp) = self.snapshot.wimp_clip else {
            return Some(clip);
        };
        let intersection = GraphicsWindow {
            left: clip.left.max(wimp.left),
            bottom: clip.bottom.max(wimp.bottom),
            right: clip.right.min(wimp.right),
            top: clip.top.min(wimp.top),
        };
        (intersection.left <= intersection.right && intersection.bottom <= intersection.top)
            .then_some(intersection)
    }

    fn text_cell_intersects_wimp_clip(&self, x: u16, y: u16) -> bool {
        self.snapshot.wimp_clip.is_none_or(|clip| {
            graphics_regions_intersect(text_cell_bounds(x, y, self.snapshot.mode), clip)
        })
    }

    fn clear_text_cells_in_graphics_region(&mut self, region: GraphicsWindow) {
        for y in self.snapshot.text_window.top..=self.snapshot.text_window.bottom {
            for x in self.snapshot.text_window.left..=self.snapshot.text_window.right {
                if !graphics_regions_intersect(text_cell_bounds(x, y, self.snapshot.mode), region) {
                    continue;
                }
                let index =
                    usize::from(y) * usize::from(self.snapshot.mode.text_columns) + usize::from(x);
                self.snapshot.text_cells[index] = b' ';
            }
        }
    }

    fn clear_graphics(&mut self) {
        if let Some(clip) = self.snapshot.wimp_clip {
            self.clear_graphics_region(clip, 0, true);
            return;
        }
        self.snapshot.primitives.clear();
        if let Some(surface) = &self.snapshot.raster_surface {
            surface.clear();
        }
        self.snapshot.graphics_content_present = false;
        self.snapshot.revision = self.snapshot.revision.wrapping_add(1);
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

fn text_cell_bounds(x: u16, y: u16, mode: ScreenMode) -> GraphicsWindow {
    let columns = i64::from(mode.text_columns.max(1));
    let rows = i64::from(mode.text_rows.max(1));
    let logical_width = i64::from(mode.logical_width.max(1));
    let logical_height = i64::from(mode.logical_height.max(1));
    let x = i64::from(x);
    let y = i64::from(y);
    let left = x * logical_width / columns;
    let right = (((x + 1) * logical_width / columns) - 1).max(left);
    let bottom = logical_height - (y + 1) * logical_height / rows;
    let top = (logical_height - y * logical_height / rows - 1).max(bottom);
    GraphicsWindow {
        left: left.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32,
        bottom: bottom.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32,
        right: right.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32,
        top: top.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32,
    }
}

fn graphics_regions_intersect(a: GraphicsWindow, b: GraphicsWindow) -> bool {
    a.left <= b.right && a.right >= b.left && a.bottom <= b.top && a.top >= b.bottom
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
        profile: GraphicsProfile::Hosted,
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

fn screen_mode_for_profile(number: u8, profile: GraphicsProfile) -> Option<ScreenMode> {
    match profile {
        GraphicsProfile::Hosted => screen_mode(number),
        GraphicsProfile::Agon => agon_screen_mode(number),
    }
}

fn agon_screen_mode(number: u8) -> Option<ScreenMode> {
    // Agon VDP 1.04+ screen modes. The hosted adapter uses the Agon's
    // 1280x1024 logical coordinate space. Double-buffered aliases currently
    // share their base mode's hosted surface without a VDP swap cycle.
    let base_number = match number {
        129 => 1,
        130 => 2,
        132 => 4,
        133 => 5,
        134 => 6,
        136 => 8,
        137 => 9,
        138 => 10,
        139 => 11,
        140 => 12,
        141 => 13,
        142 => 14,
        143 => 15,
        145 => 17,
        146 => 18,
        149 => 21,
        150 => 22,
        151 => 23,
        153 => 25,
        154 => 26,
        156 => 28,
        157 => 29,
        158 => 30,
        128..=255 => return None,
        _ => number,
    };
    if base_number == 7 {
        return Some(ScreenMode {
            number,
            profile: GraphicsProfile::Agon,
            logical_width: 0,
            logical_height: 0,
            pixel_width: 640,
            pixel_height: 480,
            text_columns: 40,
            text_rows: 25,
            colours: 16,
            bits_per_pixel: 0,
            graphics_enabled: false,
        });
    }
    let (pixel_width, pixel_height, colours) = match base_number {
        0 => (640, 480, 16),
        1 => (640, 480, 4),
        2 => (640, 480, 2),
        3 => (640, 240, 64),
        4 => (640, 240, 16),
        5 => (640, 240, 4),
        6 => (640, 240, 2),
        7 => (640, 480, 16),
        8 => (320, 240, 64),
        9 => (320, 240, 16),
        10 => (320, 240, 4),
        11 => (320, 240, 2),
        12 => (320, 200, 64),
        13 => (320, 200, 16),
        14 => (320, 200, 4),
        15 => (320, 200, 2),
        16 => (800, 600, 4),
        17 => (800, 600, 2),
        18 => (1024, 768, 2),
        19 => (1024, 768, 4),
        20 => (512, 384, 64),
        21 => (512, 384, 16),
        22 => (512, 384, 4),
        23 => (512, 384, 2),
        24 => (640, 512, 16),
        25 => (640, 512, 4),
        26 => (640, 512, 2),
        27 => (640, 256, 64),
        28 => (640, 256, 16),
        29 => (640, 256, 4),
        30 => (640, 256, 2),
        _ => return None,
    };
    let text_columns = u16::try_from((pixel_width / 8).max(1)).ok()?;
    let text_rows = u16::try_from((pixel_height / 8).max(1)).ok()?;
    let bits_per_pixel = match colours {
        2 => 1,
        4 => 2,
        16 => 4,
        64 => 6,
        _ => unreachable!("Agon screen modes use documented indexed colour counts"),
    };
    Some(ScreenMode {
        number,
        profile: GraphicsProfile::Agon,
        logical_width: 1280,
        logical_height: 1024,
        pixel_width,
        pixel_height,
        text_columns,
        text_rows,
        colours,
        bits_per_pixel,
        graphics_enabled: true,
    })
}

fn rasterize_primitive(
    surface: &SharedRasterSurface,
    snapshot: &GraphicsSnapshot,
    primitive: &GraphicsPrimitive,
) {
    match primitive {
        GraphicsPrimitive::ClearRectangle {
            bounds,
            logical_colour,
        } => {
            surface.fill_rect(
                logical_rect_pixels(*bounds, snapshot),
                graphics_colour(*logical_colour, snapshot.mode),
            );
        }
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

pub(crate) fn logical_rect_pixels(
    bounds: GraphicsWindow,
    snapshot: &GraphicsSnapshot,
) -> (u32, u32, u32, u32) {
    let mode = snapshot.mode;
    let width = i64::from(mode.pixel_width);
    let height = i64::from(mode.pixel_height);
    let logical_width = i64::from(mode.logical_width.max(1));
    let logical_height = i64::from(mode.logical_height.max(1));
    // GraphicsWindow bounds are inclusive; raster loops use exclusive ends.
    // Convert both corners to pixel indices before extending the upper ends.
    let x0 = i64::from(bounds.left).max(0);
    let x1 = i64::from(bounds.right).min(logical_width - 1);
    let y0 = i64::from(bounds.bottom).max(0);
    let y1 = i64::from(bounds.top).min(logical_height - 1);
    if x0 > x1 || y0 > y1 {
        return (0, 0, 0, 0);
    }
    let left = x0 * width / logical_width;
    let right = x1 * width / logical_width + 1;
    let top = (logical_height - 1 - y1) * height / logical_height;
    let bottom = (logical_height - 1 - y0) * height / logical_height + 1;
    (left as u32, top as u32, right as u32, bottom as u32)
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
        if mode.profile == GraphicsProfile::Agon && mode.colours == 64 {
            let colour = value % 64;
            let channel = |shift: u32| (((colour >> shift) & 3) * 85) as u8;
            return [channel(4), channel(2), channel(0), 0xFF];
        }
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
    if mode.bits_per_pixel == 8 || (mode.profile == GraphicsProfile::Agon && mode.colours == 64) {
        63
    } else {
        7
    }
}

fn is_teletext_mode(number: u8) -> bool {
    number == 7
}

fn default_palette(mode: ScreenMode) -> Vec<[u8; 4]> {
    if mode.bits_per_pixel == 32 {
        return Vec::new();
    }
    (0..mode.colours.min(256))
        .map(|logical| graphics_colour(logical, mode))
        .collect()
}

#[cfg(test)]
mod output_surface_tests {
    use super::{GraphicsService, GraphicsWindow, text_cell_bounds};

    #[test]
    fn adjacent_redraw_clears_cover_every_pixel_without_seams() {
        for mode in [0, 20] {
            let mut graphics = GraphicsService::default();
            graphics.set_mode(mode).unwrap();
            let snapshot = graphics.snapshot();
            let w = snapshot.mode.logical_width;
            let h = snapshot.mode.logical_height;
            let pw = snapshot.mode.pixel_width as usize;
            let ph = snapshot.mode.pixel_height as usize;
            let mut covered = vec![false; pw * ph];
            // Uneven split edges reproduce incremental resize damage regions.
            for (left, right) in [(0, 538), (539, w - 1)] {
                for (bottom, top) in [(0, 254), (255, h - 1)] {
                    let (x0, y0, x1, y1) = super::logical_rect_pixels(
                        GraphicsWindow {
                            left,
                            right,
                            bottom,
                            top,
                        },
                        snapshot,
                    );
                    for y in y0..y1 {
                        for x in x0..x1 {
                            covered[y as usize * pw + x as usize] = true;
                        }
                    }
                }
            }
            assert!(
                covered.iter().all(|pixel| *pixel),
                "redraw gap in MODE {mode}"
            );
            assert_eq!(
                super::logical_rect_pixels(
                    GraphicsWindow {
                        left: w,
                        right: w + 10,
                        bottom: 0,
                        top: h
                    },
                    snapshot
                ),
                (0, 0, 0, 0)
            );
        }
    }

    #[test]
    fn wimp_text_clips_and_clears_are_cell_scoped_and_window_state_is_independent() {
        let mut default = GraphicsService::default();
        default.write_byte(b'A').unwrap();
        default.write_byte(b'B').unwrap();
        assert_eq!(&default.snapshot().text_cells[..2], b"AB");

        let mut window = default.new_window_output();
        assert_eq!(&window.snapshot().text_cells[..2], b"  ");
        let second_cell = text_cell_bounds(1, 0, window.snapshot().mode);
        window.write_byte(b'A').unwrap();
        window.write_byte(b'B').unwrap();
        window.clear_wimp_region(text_cell_bounds(0, 0, window.snapshot().mode), 7);
        assert_eq!(&window.snapshot().text_cells[..2], b" B");

        window.set_wimp_redraw_clip(Some(second_cell));
        for byte in [31, 0, 0, b'X', b'Y'] {
            window.write_byte(byte).unwrap();
        }
        assert_eq!(&window.snapshot().text_cells[..2], b" Y");
        assert_eq!(&default.snapshot().text_cells[..2], b"AB");
    }

    #[test]
    fn wimp_background_clear_preserves_explicit_classic_text_colour() {
        let mut graphics = GraphicsService::default();
        let mode = graphics.snapshot().mode;
        graphics.clear_wimp_region(
            GraphicsWindow {
                left: 0,
                bottom: 0,
                right: mode.logical_width - 1,
                top: mode.logical_height - 1,
            },
            7,
        );
        // Classic VDU text obeys the guest palette, not a desktop ink override.
        graphics.write_byte(17).unwrap();
        graphics.write_byte(0).unwrap();
        graphics.write_byte(b'A').unwrap();
        assert!(!graphics.snapshot().graphics_content_present);

        let mut frame = vec![0; mode.pixel_width as usize * mode.pixel_height as usize * 4];
        crate::renderer::render_desktop_content(graphics.snapshot(), &mut frame);
        assert!(
            frame
                .chunks_exact(4)
                .any(|pixel| { pixel[0] < 100 && pixel[1] < 100 && pixel[2] < 100 })
        );
    }
}
