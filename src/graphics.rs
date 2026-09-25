use crate::error::RuntimeError;

const DEFAULT_GRAPHICS_WIDTH: i32 = 1280;
const DEFAULT_GRAPHICS_HEIGHT: i32 = 1024;

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
    pub text_columns: u16,
    pub text_rows: u16,
    pub colours: u8,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum GraphicsPrimitive {
    Line {
        from: Point,
        to: Point,
        plot_code: u8,
        action: u8,
        logical_colour: u8,
        clip: GraphicsWindow,
    },
    Point {
        at: Point,
        plot_code: u8,
        action: u8,
        logical_colour: u8,
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
    pub graphics_colour: u8,
    pub text_cells: Vec<u8>,
    pub primitives: Vec<GraphicsPrimitive>,
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
                text_colour: 7,
                graphics_action: 0,
                graphics_colour: 7,
                text_cells: vec![
                    b' ';
                    usize::from(mode.text_columns) * usize::from(mode.text_rows)
                ],
                primitives: Vec::new(),
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
                    self.snapshot.primitives.push(GraphicsPrimitive::Line {
                        from,
                        to: target,
                        plot_code,
                        action: self.snapshot.graphics_action,
                        logical_colour: self.snapshot.graphics_colour,
                        clip: self.snapshot.graphics_window,
                    });
                    self.snapshot.graphics_cursor = target;
                }
            }
            64 => {
                if operation == 0 {
                    self.snapshot.graphics_cursor = target;
                } else {
                    self.snapshot.primitives.push(GraphicsPrimitive::Point {
                        at: target,
                        plot_code,
                        action: self.snapshot.graphics_action,
                        logical_colour: self.snapshot.graphics_colour,
                        clip: self.snapshot.graphics_window,
                    });
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
            16 => self.snapshot.primitives.clear(),
            17 => self.snapshot.text_colour = parameters[0],
            18 => {
                self.snapshot.graphics_action = parameters[0];
                self.snapshot.graphics_colour = parameters[1];
            }
            20 => {
                self.snapshot.text_colour = 7;
                self.snapshot.graphics_action = 0;
                self.snapshot.graphics_colour = 7;
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
        self.snapshot.text_colour = 7;
        self.snapshot.graphics_action = 0;
        self.snapshot.graphics_colour = 7;
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
    let (logical_height, text_columns, text_rows, colours) = match number {
        0 => (DEFAULT_GRAPHICS_HEIGHT, 80, 32, 2),
        1 => (DEFAULT_GRAPHICS_HEIGHT, 40, 32, 4),
        2 => (DEFAULT_GRAPHICS_HEIGHT, 20, 32, 16),
        3 => (1000, 80, 25, 2),
        4 => (DEFAULT_GRAPHICS_HEIGHT, 40, 32, 2),
        5 => (DEFAULT_GRAPHICS_HEIGHT, 20, 32, 4),
        6 => (1000, 40, 25, 2),
        7 => (1000, 40, 25, 16),
        _ => return None,
    };
    Some(ScreenMode {
        number,
        logical_width: DEFAULT_GRAPHICS_WIDTH,
        logical_height,
        text_columns,
        text_rows,
        colours,
    })
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
    GraphicsWindow {
        left: 0,
        bottom: 0,
        right: mode.logical_width - 1,
        top: mode.logical_height - 1,
    }
}
