use crate::{
    configure::{BasicConfiguration, BasicEngine, ConfigureStore, StartupLanguage},
    error::RuntimeError,
    filesystem::{
        FILETYPE_BASIC, FILETYPE_BASIC64, FILETYPE_TEXT, FileMetadata, HostFileSystem, OpenFile,
    },
    graphics::{GraphicsProfile, GraphicsService, GraphicsSnapshot, GraphicsWindow},
    host::HostConsole,
    memory::{GUEST_MEMORY_BASE, Task},
};
use std::{
    collections::HashMap,
    io::{Read, Seek, SeekFrom, Write},
    sync::mpsc::Sender,
    time::{Duration, Instant},
};

use std::sync::Arc;

mod mos;
pub(crate) use mos::MosClock;

use crate::wimp::{
    WIMP_CLOSE_DOWN, WIMP_CLOSE_WINDOW, WIMP_CREATE_ICON, WIMP_CREATE_ICON_EX, WIMP_CREATE_MENU,
    WIMP_CREATE_WINDOW, WIMP_DELETE_ICON, WIMP_FORCE_REDRAW, WIMP_GET_POINTER_INFO,
    WIMP_GET_RECTANGLE, WIMP_GET_WINDOW_STATE, WIMP_INITIALISE, WIMP_OPEN_WINDOW, WIMP_POLL,
    WIMP_REDRAW_WINDOW, WIMP_SET_EXTENT, WIMP_SET_ICON_STATE, WIMP_START_TASK, WIMP_UPDATE_WINDOW,
    WimpServer, WorkArea,
};

const DISPLAY_BATCH_FRAME_INTERVAL: Duration = Duration::from_micros(16_667);
const INKEY_POLL_INTERVAL: Duration = Duration::from_millis(8);
const MAX_EXTENDED_MODE_PIXELS: u64 = 4_194_304;
const MAX_TASK_GRAPHICS_PIXELS: u64 = 8_388_608;

pub const OS_WRITE_C: u32 = 0x00;
pub const OS_WRITE_S: u32 = 0x01;
pub const OS_WRITE_0: u32 = 0x02;
pub const OS_NEW_LINE: u32 = 0x03;
pub const OS_READ_C: u32 = 0x04;
pub const OS_CLI: u32 = 0x05;
pub const OS_BYTE: u32 = 0x06;
pub const OS_WORD: u32 = 0x07;
pub const OS_FILE: u32 = 0x08;
pub const OS_ARGS: u32 = 0x09;
pub const OS_BGET: u32 = 0x0A;
pub const OS_BPUT: u32 = 0x0B;
pub const OS_GBPB: u32 = 0x0C;
pub const OS_FIND: u32 = 0x0D;
pub const OS_READ_LINE: u32 = 0x0E;
pub const OS_FSCONTROL: u32 = 0x29;
pub const OS_READ_POINT: u32 = 0x32;
pub const OS_PLOT: u32 = 0x45;
pub use crate::wimp::{
    WIMP_CLOSE_DOWN as WIMP_CLOSE_DOWN_SWI, WIMP_CLOSE_WINDOW as WIMP_CLOSE_WINDOW_SWI,
    WIMP_CREATE_ICON as WIMP_CREATE_ICON_SWI, WIMP_CREATE_ICON_EX as WIMP_CREATE_ICON_EX_SWI,
    WIMP_CREATE_MENU as WIMP_CREATE_MENU_SWI, WIMP_CREATE_WINDOW as WIMP_CREATE_WINDOW_SWI,
    WIMP_DELETE_ICON as WIMP_DELETE_ICON_SWI, WIMP_FORCE_REDRAW as WIMP_FORCE_REDRAW_SWI,
    WIMP_GET_POINTER_INFO as WIMP_GET_POINTER_INFO_SWI,
    WIMP_GET_RECTANGLE as WIMP_GET_RECTANGLE_SWI,
    WIMP_GET_WINDOW_STATE as WIMP_GET_WINDOW_STATE_SWI, WIMP_INITIALISE as WIMP_INITIALISE_SWI,
    WIMP_OPEN_WINDOW as WIMP_OPEN_WINDOW_SWI, WIMP_POLL as WIMP_POLL_SWI,
    WIMP_REDRAW_WINDOW as WIMP_REDRAW_WINDOW_SWI, WIMP_SET_EXTENT as WIMP_SET_EXTENT_SWI,
    WIMP_SET_ICON_STATE as WIMP_SET_ICON_STATE_SWI, WIMP_START_TASK as WIMP_START_TASK_SWI,
    WIMP_UPDATE_WINDOW as WIMP_UPDATE_WINDOW_SWI,
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DisplayEvent {
    WriteByte {
        task_id: u64,
        window_handle: Option<u32>,
        byte: u8,
    },
    Plot {
        task_id: u64,
        window_handle: Option<u32>,
        code: u8,
        x: i32,
        y: i32,
    },
    GraphicsSnapshot {
        task_id: u64,
        window_handle: Option<u32>,
        snapshot: GraphicsSnapshot,
    },
    DesktopStarted,
    DesktopChanged,
    RuntimeExited,
}

const R0: usize = 0;
const R1: usize = 1;
const R2: usize = 2;
const R3: usize = 3;
const R4: usize = 4;
const R5: usize = 5;
const R6: usize = 6;
const HOST_FS_NUMBER: u32 = 1;
const HOST_FS_CONTROL_BLOCK: u32 = GUEST_MEMORY_BASE;
const GUEST_ADDRESS_MASK: u32 = 0x3FFF_FFFF;
const READ_LINE_ECHO_ONLY_BUFFERED: u32 = 1 << 31;
const READ_LINE_ECHO_R4: u32 = 1 << 30;
const MAX_CLI_BYTES: usize = 256;
const MAX_STRING_BYTES: usize = 4096;
const OUTPUT_BUFFER: u32 = GUEST_MEMORY_BASE + 0x1000;
const CLI_STRING_BUFFER: u32 = GUEST_MEMORY_BASE + 0x3000;
const HELP_TEXT: &[u8] = b"Acorn-2026 MOS commands:\n\r  Commands can be abbreviated with a final dot (for example, *CA. and *CONF.); *. is a shortcut for *CAT.\n\r  *CAT [dir]             Catalogue a directory.\n\r  *DIR [dir]             Select the current directory.\n\r  *CDIR <dir>            Create a directory.\n\r  *DELETE <file>         Delete a file.\n\r  *RENAME <old> <new>    Rename a file or directory.\n\r  *FILETYPE <file> <id>  Set a RISC OS file type.\n\r  *TYPE <file>           Display a text file.\n\r  *DISC [name]           Read or set the volume name.\n\r  *HOSTFS                Select the HostFS filing system.\n\r  *CONFIGURE <option> <value> Save a BASIC or startup preference.\n\r  *CONFIGURE Language 0  Open the MOS prompt on load.\n\r  *CONFIGURE Language 3  Open the desktop on load.\n\r  *CONFIGURE DEFAULTS    Restore configuration defaults.\n\r  *STATUS [option]       Show saved configuration.\n\r  *BASIC <file>          Load and run BASIC with saved preferences.\n\r  DESKTOP                Start the hosted Wimp desktop.\n\r  RUN <file>             Run a BASIC source or tokenised file.\n\r  BASICLOAD <file>       Load a tokenised BASIC program.\n\r  BASICRUN               Run the loaded program.\n\r  BASICJIT [file]        Run with experimental native hot regions.\n\r  BASICJIT STRICT [file] Compile and run supported code without fallback.\n\r  HELP                   Show this help.\n\r  QUIT                   Exit the runtime.";

fn work_area_to_graphics_clip(
    work: WorkArea,
    extent: WorkArea,
    logical_height: i32,
) -> GraphicsWindow {
    let left = i64::from(work.min_x) - i64::from(extent.min_x);
    let right = i64::from(work.max_x) - 1 - i64::from(extent.min_x);
    let bottom = i64::from(logical_height) + i64::from(work.min_y) - i64::from(extent.max_y);
    let top = i64::from(logical_height) + i64::from(work.max_y) - 1 - i64::from(extent.max_y);
    GraphicsWindow {
        left: left.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32,
        bottom: bottom.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32,
        right: right.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32,
        top: top.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32,
    }
}

fn hsv_to_rgb(hue: f64, saturation: f64, value: f64) -> (u8, u8, u8) {
    let hue = hue.rem_euclid(360.0) / 60.0;
    let chroma = value * saturation.clamp(0.0, 1.0);
    let secondary = chroma * (1.0 - (hue.rem_euclid(2.0) - 1.0).abs());
    let (red, green, blue) = match hue as u8 {
        0 => (chroma, secondary, 0.0),
        1 => (secondary, chroma, 0.0),
        2 => (0.0, chroma, secondary),
        3 => (0.0, secondary, chroma),
        4 => (secondary, 0.0, chroma),
        _ => (chroma, 0.0, secondary),
    };
    let match_value = value - chroma;
    let component = |channel: f64| ((channel + match_value) * 255.0).round() as u8;
    (component(red), component(green), component(blue))
}

#[derive(Clone, Copy, Debug, Default)]
pub struct SwiContext {
    pub registers: [u32; 16],
    /// For OS_WriteS, this is the caller's saved return address: the first
    /// byte immediately following the SWI instruction.
    pub pc: u32,
    pub carry: bool,
}

pub struct SwiDispatcher {
    mos: mos::MosState,
    configure: ConfigureStore,
    console: HostConsole,
    graphics: GraphicsService,
    window_graphics: HashMap<u32, GraphicsService>,
    active_graphics_window: Option<u32>,
    file_system: HostFileSystem,
    quit_requested: bool,
    desktop_requested: bool,
    display_events: Option<Sender<DisplayEvent>>,
    display_task_id: u64,
    wimp: Option<Arc<WimpServer>>,
    desktop_service: Option<Arc<WimpServer>>,
    display_batch_active: bool,
    last_display_batch_publish: Option<Instant>,
    last_inkey_poll: Instant,
}

impl SwiDispatcher {
    pub fn new(console: HostConsole) -> Self {
        Self::with_display_events(console, None, 1, None)
    }

    pub fn windowed(console: HostConsole, display_events: Sender<DisplayEvent>) -> Self {
        Self::with_display_events(console, Some(display_events), 1, None)
    }

    pub(crate) fn windowed_with_desktop(
        console: HostConsole,
        display_events: Sender<DisplayEvent>,
        wimp: Arc<WimpServer>,
    ) -> Self {
        let mut dispatcher = Self::with_display_events(console, Some(display_events), 1, None);
        dispatcher.desktop_service = Some(wimp);
        dispatcher
    }

    pub(crate) fn desktop_task(
        console: HostConsole,
        display_events: Sender<DisplayEvent>,
        task_id: u64,
        wimp: Arc<WimpServer>,
    ) -> Self {
        Self::with_display_events(console, Some(display_events), task_id, Some(wimp))
    }

    fn with_display_events(
        console: HostConsole,
        display_events: Option<Sender<DisplayEvent>>,
        display_task_id: u64,
        wimp: Option<Arc<WimpServer>>,
    ) -> Self {
        Self {
            mos: mos::MosState::default(),
            configure: ConfigureStore::default(),
            console,
            graphics: GraphicsService::default(),
            window_graphics: HashMap::new(),
            active_graphics_window: None,
            file_system: HostFileSystem::demo_default(),
            quit_requested: false,
            desktop_requested: false,
            display_events,
            display_task_id,
            wimp,
            desktop_service: None,
            display_batch_active: false,
            last_display_batch_publish: None,
            last_inkey_poll: Instant::now(),
        }
    }

    pub fn quit_requested(&self) -> bool {
        self.quit_requested
    }

    pub fn desktop_requested(&self) -> bool {
        self.desktop_requested
    }

    pub fn graphics(&self) -> &GraphicsService {
        self.current_graphics()
    }

    fn current_graphics(&self) -> &GraphicsService {
        self.active_graphics_window
            .and_then(|handle| self.window_graphics.get(&handle))
            .unwrap_or(&self.graphics)
    }

    fn current_graphics_mut(&mut self) -> &mut GraphicsService {
        if let Some(handle) = self.active_graphics_window {
            if let Some(graphics) = self.window_graphics.get_mut(&handle) {
                return graphics;
            }
        }
        &mut self.graphics
    }

    fn ensure_graphics_pixel_budget(
        &self,
        target_window: Option<u32>,
        replacement_pixels: u64,
    ) -> Result<(), RuntimeError> {
        let mut total = if target_window.is_none() {
            replacement_pixels
        } else {
            self.graphics.mode_pixel_count()
        };
        for (handle, graphics) in &self.window_graphics {
            total = total.saturating_add(if Some(*handle) == target_window {
                replacement_pixels
            } else {
                graphics.mode_pixel_count()
            });
        }
        if let Some(handle) = target_window {
            if !self.window_graphics.contains_key(&handle) {
                total = total.saturating_add(replacement_pixels);
            }
        }
        if total > MAX_TASK_GRAPHICS_PIXELS {
            return Err(RuntimeError::Program(
                "hosted Wimp graphics surface budget for this task is exhausted".into(),
            ));
        }
        Ok(())
    }

    pub(crate) fn load_basic_configuration(&self) -> Result<BasicConfiguration, RuntimeError> {
        self.effective_configure_store()
            .load()
            .map_err(RuntimeError::Program)
    }

    pub(crate) fn desktop_is_configured_for_startup(&self) -> Result<bool, RuntimeError> {
        if self.desktop_service.is_none() {
            return Ok(false);
        }
        Ok(self.load_basic_configuration()?.startup_language == StartupLanguage::Desktop)
    }

    fn effective_configure_store(&self) -> ConfigureStore {
        self.wimp
            .as_ref()
            .and_then(|wimp| wimp.configure_store())
            .unwrap_or_else(|| self.configure.clone())
    }

    #[cfg(test)]
    pub(crate) fn set_configure_store_for_test(&mut self, configure: ConfigureStore) {
        self.configure = configure;
    }

    #[cfg(test)]
    pub(crate) fn set_file_system_for_test(&mut self, file_system: HostFileSystem) {
        self.file_system = file_system;
    }

    pub(crate) fn set_graphics_profile(
        &mut self,
        profile: GraphicsProfile,
    ) -> Result<(), RuntimeError> {
        let previous = self.current_graphics().snapshot().clone();
        self.current_graphics_mut().set_profile(profile)?;
        let snapshot = self.current_graphics().snapshot().clone();
        if snapshot != previous {
            self.publish_snapshot(snapshot);
        }
        Ok(())
    }

    pub(crate) fn poll_key(&mut self) -> Option<u8> {
        if self.last_inkey_poll.elapsed() < INKEY_POLL_INTERVAL {
            return None;
        }
        self.last_inkey_poll = Instant::now();
        self.mos
            .input
            .pop_front()
            .or_else(|| self.console.try_read_byte())
    }

    pub(crate) fn set_mode_from_block(
        &mut self,
        task: &Task,
        address: u32,
    ) -> Result<(), RuntimeError> {
        let read_word = |offset: u32| -> Result<i32, RuntimeError> {
            let word_address = address
                .checked_add(offset)
                .ok_or(crate::memory::MemoryError::AddressOverflow)?;
            let bytes = [
                task.memory.read_byte(word_address)?,
                task.memory.read_byte(word_address + 1)?,
                task.memory.read_byte(word_address + 2)?,
                task.memory.read_byte(word_address + 3)?,
            ];
            Ok(i32::from_le_bytes(bytes))
        };
        if read_word(0)? != 1 || read_word(36)? != -1 {
            return Err(RuntimeError::Program(
                "unsupported extended mode block format".into(),
            ));
        }
        let width = read_word(4)?;
        let height = read_word(8)?;
        let depth = read_word(12)?;
        let y_eigenfactor = read_word(24)?;
        let x_eigenfactor = read_word(32)?;
        if depth != 5 {
            return Err(RuntimeError::Program(
                "the hosted extended mode currently supports 32-bit colour".into(),
            ));
        }
        if width <= 0
            || height <= 0
            || u64::try_from(width).unwrap_or(u64::MAX) * u64::try_from(height).unwrap_or(u64::MAX)
                > MAX_EXTENDED_MODE_PIXELS
            || !(0..=4).contains(&x_eigenfactor)
            || !(0..=4).contains(&y_eigenfactor)
        {
            return Err(RuntimeError::Program(
                "extended mode dimensions or eigenfactors are outside the hosted profile".into(),
            ));
        }
        self.ensure_graphics_pixel_budget(
            self.active_graphics_window,
            u64::from(width as u32) * u64::from(height as u32),
        )?;
        self.current_graphics_mut().set_extended_mode(
            width as u32,
            height as u32,
            x_eigenfactor as u8,
            y_eigenfactor as u8,
        )
    }

    pub(crate) fn dispatch_named_swi(
        &mut self,
        name: &str,
        task: &mut Task,
        context: &mut SwiContext,
    ) -> Result<(), RuntimeError> {
        match name {
            "OS_BYTE" => self.dispatch(OS_BYTE, task, context),
            "OS_WORD" => self.dispatch(OS_WORD, task, context),
            "OS_WRITEC" => self.dispatch(OS_WRITE_C, task, context),
            "OS_NEWLINE" => self.dispatch(OS_NEW_LINE, task, context),
            "OS_READC" => self.dispatch(OS_READ_C, task, context),
            "OS_READPOINT" => self.dispatch(OS_READ_POINT, task, context),
            "OS_CLI" => self.dispatch(OS_CLI, task, context),
            "OS_FILE" => self.os_file(task, context),
            "OS_ARGS" => self.os_args(task, context),
            "OS_BGET" => self.os_bget(task, context),
            "OS_BPUT" => self.os_bput(task, context),
            "OS_GBPB" => self.os_gbpb(task, context),
            "OS_FIND" => self.os_find(task, context),
            "OS_FSCONTROL" => self.os_fscontrol(task, context),
            "COLOURTRANS_CONVERTHSVTORGB" => {
                let hue = f64::from(context.registers[R0] as i32) / 65_536.0;
                let saturation = f64::from(context.registers[R1]) / 65_280.0;
                let value = f64::from(context.registers[R2] & 0xFF) / 255.0;
                let (red, green, blue) = hsv_to_rgb(hue, saturation, value);
                context.registers[R0] = u32::from(red);
                context.registers[R1] = u32::from(green);
                context.registers[R2] = u32::from(blue);
                Ok(())
            }
            "COLOURTRANS_SETGCOL" => {
                self.current_graphics_mut()
                    .set_rgb_gcol(context.registers[R0]);
                Ok(())
            }
            "COLOURTRANS_WRITEPALETTE" => Ok(()),
            "WIMP_INITIALISE" => self.dispatch_wimp(WIMP_INITIALISE, task, context),
            "WIMP_CREATEWINDOW" => self.dispatch_wimp(WIMP_CREATE_WINDOW, task, context),
            "WIMP_CREATEICON" => self.dispatch_wimp(WIMP_CREATE_ICON, task, context),
            "WIMP_CREATEICONEX" => self.dispatch_wimp(WIMP_CREATE_ICON_EX, task, context),
            "WIMP_CREATEMENU" => self.dispatch_wimp(WIMP_CREATE_MENU, task, context),
            "WIMP_DELETEICON" => self.dispatch_wimp(WIMP_DELETE_ICON, task, context),
            "WIMP_OPENWINDOW" => self.dispatch_wimp(WIMP_OPEN_WINDOW, task, context),
            "WIMP_REDRAWWINDOW" => self.dispatch_wimp(WIMP_REDRAW_WINDOW, task, context),
            "WIMP_UPDATEWINDOW" => self.dispatch_wimp(WIMP_UPDATE_WINDOW, task, context),
            "WIMP_GETRECTANGLE" => self.dispatch_wimp(WIMP_GET_RECTANGLE, task, context),
            "WIMP_FORCEREDRAW" => self.dispatch_wimp(WIMP_FORCE_REDRAW, task, context),
            "WIMP_CLOSEWINDOW" => self.dispatch_wimp(WIMP_CLOSE_WINDOW, task, context),
            "WIMP_POLL" => self.dispatch_wimp(WIMP_POLL, task, context),
            "WIMP_GETWINDOWSTATE" => self.dispatch_wimp(WIMP_GET_WINDOW_STATE, task, context),
            "WIMP_GETPOINTERINFO" => self.dispatch_wimp(WIMP_GET_POINTER_INFO, task, context),
            "WIMP_SETICONSTATE" => self.dispatch_wimp(WIMP_SET_ICON_STATE, task, context),
            "WIMP_SETEXTENT" => self.dispatch_wimp(WIMP_SET_EXTENT, task, context),
            "WIMP_CLOSEDOWN" => self.dispatch_wimp(WIMP_CLOSE_DOWN, task, context),
            "WIMP_STARTTASK" => self.dispatch_wimp(WIMP_START_TASK, task, context),
            "ACORN_DESKTOP" => self.acorn_desktop(task, context),
            _ => Err(RuntimeError::Program(format!(
                "named SWI {name} is not available in the hosted profile"
            ))),
        }
    }

    pub fn dispatch(
        &mut self,
        number: u32,
        task: &mut Task,
        context: &mut SwiContext,
    ) -> Result<(), RuntimeError> {
        if matches!(
            number,
            WIMP_INITIALISE
                | WIMP_CREATE_WINDOW
                | WIMP_REDRAW_WINDOW
                | WIMP_UPDATE_WINDOW
                | WIMP_GET_RECTANGLE
                | WIMP_FORCE_REDRAW
                | WIMP_CREATE_ICON
                | WIMP_CREATE_ICON_EX
                | WIMP_DELETE_ICON
                | WIMP_OPEN_WINDOW
                | WIMP_CLOSE_WINDOW
                | WIMP_POLL
                | WIMP_GET_WINDOW_STATE
                | WIMP_SET_ICON_STATE
                | WIMP_GET_POINTER_INFO
                | WIMP_CREATE_MENU
                | WIMP_SET_EXTENT
                | WIMP_CLOSE_DOWN
                | WIMP_START_TASK
        ) {
            return self.dispatch_wimp(number, task, context);
        }
        match number {
            OS_WRITE_C => {
                let character = context.registers[R0] as u8;
                if let Some(mode) = self.current_graphics().mode_after_vdu_byte(character) {
                    self.ensure_graphics_pixel_budget(
                        self.active_graphics_window,
                        u64::from(mode.pixel_width) * u64::from(mode.pixel_height),
                    )?;
                }
                let previous_mode = self.current_graphics().snapshot().mode;
                let output_byte = self.current_graphics_mut().write_byte(character)?;
                let mode_changed = self.current_graphics().snapshot().mode != previous_mode;
                if mode_changed {
                    self.publish_snapshot(self.current_graphics().snapshot().clone());
                } else if !self.display_batch_active || output_byte.is_some() {
                    self.publish_display_event(DisplayEvent::WriteByte {
                        task_id: self.display_task_id,
                        window_handle: self.active_graphics_window,
                        byte: character,
                    });
                } else {
                    self.publish_snapshot(self.current_graphics().snapshot().clone());
                }
                if let Some(byte) = output_byte {
                    self.console.write_byte(byte)?;
                }
                self.console.flush().map_err(RuntimeError::from)
            }
            OS_WRITE_S => self.write_inline_string(task, context),
            OS_WRITE_0 => self.write_indirect_string(task, context),
            OS_NEW_LINE => {
                self.emit_via_write_c(task, b'\n')?;
                self.emit_via_write_c(task, b'\r')
            }
            OS_READ_C => self.read_character(context),
            OS_CLI => self.execute_cli(task, context),
            OS_BYTE => self.os_byte(context),
            OS_WORD => self.os_word(task, context),
            OS_READ_LINE => self.read_line(task, context),
            OS_FILE => self.os_file(task, context),
            OS_ARGS => self.os_args(task, context),
            OS_BGET => self.os_bget(task, context),
            OS_BPUT => self.os_bput(task, context),
            OS_GBPB => self.os_gbpb(task, context),
            OS_FIND => self.os_find(task, context),
            OS_FSCONTROL => self.os_fscontrol(task, context),
            OS_PLOT => {
                let code = context.registers[R0] as u8;
                let x = context.registers[R1] as i32;
                let y = context.registers[R2] as i32;
                self.current_graphics_mut().plot(code, x, y)?;
                if self.display_batch_active {
                    self.publish_display_batch_snapshot_if_due();
                } else {
                    self.publish_display_event(DisplayEvent::Plot {
                        task_id: self.display_task_id,
                        window_handle: self.active_graphics_window,
                        code,
                        x,
                        y,
                    });
                }
                Ok(())
            }
            OS_READ_POINT => {
                let x = context.registers[R0] as i32;
                let y = context.registers[R1] as i32;
                if let Some((colour, tint)) = self.current_graphics().read_point(x, y) {
                    context.registers[R2] = colour;
                    context.registers[R3] = tint;
                    context.registers[R4] = 0;
                } else {
                    context.registers[R2] = u32::MAX;
                    context.registers[R3] = 0;
                    context.registers[R4] = u32::MAX;
                }
                Ok(())
            }
            other => Err(RuntimeError::InvalidSwi(other)),
        }
    }

    fn dispatch_wimp(
        &mut self,
        number: u32,
        task: &mut Task,
        context: &mut SwiContext,
    ) -> Result<(), RuntimeError> {
        let Some(wimp) = self.wimp.clone() else {
            return Err(RuntimeError::InvalidSwi(number));
        };
        wimp.dispatch(number, task, context)?;
        if let Some((window_handle, work, extent)) = wimp.current_graphics_context(task.id) {
            if self.active_graphics_window != Some(window_handle) {
                if !self.window_graphics.contains_key(&window_handle) {
                    self.ensure_graphics_pixel_budget(
                        Some(window_handle),
                        self.graphics.mode_pixel_count(),
                    )?;
                    self.window_graphics
                        .insert(window_handle, self.graphics.new_window_output());
                }
                self.active_graphics_window = Some(window_handle);
            }
            let mode_height = self.current_graphics().snapshot().mode.logical_height;
            let clip = work_area_to_graphics_clip(work, extent, mode_height);
            self.current_graphics_mut().set_wimp_redraw_clip(Some(clip));
            if wimp.current_redraw_clears_background(task.id) {
                let background = wimp.current_redraw_background_colour(task.id).unwrap_or(7);
                self.current_graphics_mut()
                    .clear_wimp_region(clip, background);
            }
            self.publish_snapshot(self.current_graphics().snapshot().clone());
        } else if let Some(window_handle) = self.active_graphics_window.take() {
            if let Some(graphics) = self.window_graphics.get_mut(&window_handle) {
                graphics.set_wimp_redraw_clip(None);
                let snapshot = graphics.snapshot().clone();
                self.publish_snapshot_for_window(Some(window_handle), snapshot);
            }
        }
        Ok(())
    }

    /// Project extension for the BASIC64 Filer. It exposes checked HostFS
    /// catalogue records and the mounted volume name; Filer navigation and
    /// activation policy remain in BASIC64.
    fn acorn_desktop(
        &mut self,
        task: &mut Task,
        context: &mut SwiContext,
    ) -> Result<(), RuntimeError> {
        match context.registers[R0] {
            1 => {
                let path = read_guest_control_string(task, context.registers[R1])?;
                let index = context.registers[R2] as usize;
                let capacity = context.registers[R4] as usize;
                if capacity == 0 || capacity > crate::memory::GUEST_MEMORY_SIZE as usize {
                    return Err(RuntimeError::Program(
                        "Acorn_Desktop entry buffer size is outside the hosted limit".into(),
                    ));
                }
                let entries = self.file_system.enumerate(&task.file_system, &path, "*")?;
                let Some(entry) = entries.get(index) else {
                    task.memory.write_byte(context.registers[R3], 0)?;
                    context.registers[R0] = 0;
                    context.registers[R1] = 0;
                    return Ok(());
                };
                if entry.guest_name.len() + 1 > capacity {
                    return Err(RuntimeError::Program(
                        "Acorn_Desktop entry name does not fit the caller buffer".into(),
                    ));
                }
                write_guest_string(task, context.registers[R3], &entry.guest_name)?;
                context.registers[R0] = if entry.is_directory {
                    2
                } else if matches!(
                    entry.metadata.file_type & 0xFFF,
                    FILETYPE_BASIC | FILETYPE_BASIC64
                ) {
                    1
                } else if entry.metadata.file_type & 0xFFF == FILETYPE_TEXT {
                    3
                } else {
                    4
                };
                context.registers[R1] = entry.metadata.file_type;
                context.registers[R2] = entry.length;
                Ok(())
            }
            2 => {
                let capacity = context.registers[R2] as usize;
                let name = self.file_system.volume_name();
                if name.len() + 1 > capacity {
                    return Err(RuntimeError::Program(
                        "Acorn_Desktop volume name does not fit the caller buffer".into(),
                    ));
                }
                write_guest_string(task, context.registers[R1], name)?;
                context.registers[R0] = u32::try_from(name.len()).unwrap_or(u32::MAX);
                Ok(())
            }
            4 => self
                .wimp
                .as_ref()
                .ok_or(RuntimeError::InvalidSwi(0x4FF00))?
                .register_system_menu(task.id),
            3 => {
                // Additive hosted catalogue metadata: leave reason 1 unchanged.
                let path = read_guest_control_string(task, context.registers[R1])?;
                let index = context.registers[R2] as usize;
                let entries = self.file_system.enumerate(&task.file_system, &path, "*")?;
                let modified = entries.get(index).and_then(|entry| {
                    std::fs::metadata(&entry.host_path)
                        .ok()?
                        .modified()
                        .ok()?
                        .duration_since(std::time::UNIX_EPOCH)
                        .ok()
                        .and_then(|duration| u32::try_from(duration.as_secs()).ok())
                });
                context.registers[R0] = u32::from(modified.is_some());
                context.registers[R1] = modified.unwrap_or(0);
                Ok(())
            }
            action => Err(RuntimeError::Program(format!(
                "Acorn_Desktop action {action} is not supported"
            ))),
        }
    }

    fn publish_snapshot(&self, snapshot: GraphicsSnapshot) {
        self.publish_snapshot_for_window(self.active_graphics_window, snapshot);
    }

    fn publish_snapshot_for_window(&self, window_handle: Option<u32>, snapshot: GraphicsSnapshot) {
        self.publish_display_event(DisplayEvent::GraphicsSnapshot {
            task_id: self.display_task_id,
            window_handle,
            snapshot,
        });
    }

    pub(crate) fn read_guest_file(
        &self,
        task: &Task,
        path: &str,
    ) -> Result<(Vec<u8>, FileMetadata), RuntimeError> {
        self.file_system.read_file(&task.file_system, path)
    }

    pub(crate) fn set_program_working_directory(
        &self,
        task: &mut Task,
        path: &str,
    ) -> Result<(), RuntimeError> {
        let resolved = self
            .file_system
            .canonical_guest_path(&task.file_system, path)?;
        if resolved.is_directory {
            return Err(RuntimeError::Program(format!("'{path}' is a directory")));
        }
        let parent = resolved
            .guest_path
            .rsplit_once('.')
            .map(|(parent, _)| parent)
            .filter(|parent| !parent.is_empty())
            .map(|parent| format!("$.{parent}"))
            .unwrap_or_else(|| "$".to_string());
        self.file_system
            .set_current_directory(&mut task.file_system, &parent)
    }

    fn os_file(&mut self, task: &mut Task, context: &mut SwiContext) -> Result<(), RuntimeError> {
        let reason = context.registers[R0];
        let path = read_guest_string(task, context.registers[R1])?;
        match reason {
            0 | 10 => {
                let start = context.registers[R4];
                let end = context.registers[R5];
                let length = end.checked_sub(start).ok_or_else(|| {
                    RuntimeError::Program("OS_File save end precedes its start".into())
                })?;
                let bytes = task.memory.read_bytes(start, length as usize)?;
                let mut metadata = metadata_for_new_guest_path(&path);
                if reason == 0 {
                    apply_riscos_load_address(&mut metadata, context.registers[R2]);
                    metadata.execution_address = context.registers[R3];
                } else {
                    metadata.file_type = context.registers[R2] & 0xFFF;
                }
                self.file_system
                    .write_file(&task.file_system, &path, &bytes, metadata)
            }
            1 | 2 | 3 | 4 | 9 | 18 => {
                let mut metadata = self.metadata_for_guest_object(task, &path)?;
                match reason {
                    1 => {
                        apply_riscos_load_address(&mut metadata, context.registers[R2]);
                        metadata.execution_address = context.registers[R3];
                        metadata.attributes = context.registers[R5];
                    }
                    2 => apply_riscos_load_address(&mut metadata, context.registers[R2]),
                    3 => metadata.execution_address = context.registers[R3],
                    4 => metadata.attributes = context.registers[R5],
                    9 if metadata.file_type == 0 => metadata.file_type = 0xFFD,
                    18 => metadata.file_type = context.registers[R2] & 0xFFF,
                    _ => {}
                }
                self.file_system
                    .set_metadata(&task.file_system, &path, &metadata)
            }
            5 | 13 | 15 | 17 => {
                let (object_type, metadata, length) = self.catalogue_object(task, &path)?;
                context.registers[R0] = object_type;
                if object_type != 0 {
                    context.registers[R2] = riscos_load_address(&metadata);
                    context.registers[R3] = metadata.execution_address;
                    context.registers[R4] = length;
                    context.registers[R5] = metadata.attributes;
                }
                Ok(())
            }
            6 => {
                let (object_type, metadata, length) = self.catalogue_object(task, &path)?;
                context.registers[R0] = object_type;
                if object_type != 0 {
                    context.registers[R2] = riscos_load_address(&metadata);
                    context.registers[R3] = metadata.execution_address;
                    context.registers[R4] = length;
                    context.registers[R5] = metadata.attributes;
                    if object_type == 2 {
                        self.file_system
                            .remove_directory(&task.file_system, &path)?;
                    } else {
                        self.file_system.delete_file(&task.file_system, &path)?;
                    }
                }
                Ok(())
            }
            7 | 11 => {
                let mut metadata = metadata_for_new_guest_path(&path);
                if reason == 7 {
                    apply_riscos_load_address(&mut metadata, context.registers[R2]);
                    metadata.execution_address = context.registers[R3];
                } else {
                    metadata.file_type = context.registers[R2] & 0xFFF;
                }
                self.file_system
                    .write_file(&task.file_system, &path, &[], metadata)
            }
            8 => {
                match self
                    .file_system
                    .canonical_guest_path(&task.file_system, &path)
                {
                    Ok(resolved) if resolved.is_directory => Ok(()),
                    _ => self
                        .file_system
                        .create_directory(&task.file_system, &path)
                        .map(|_| ()),
                }
            }
            12 | 14 | 16 | 255 => {
                let (bytes, metadata) = self.file_system.read_file(&task.file_system, &path)?;
                let destination = if context.registers[R3] & 0xFF == 0 {
                    context.registers[R2]
                } else {
                    metadata.load_address
                };
                task.memory.write_bytes(destination, &bytes)?;
                context.registers[R0] = 1;
                context.registers[R2] = riscos_load_address(&metadata);
                context.registers[R3] = metadata.execution_address;
                context.registers[R4] = u32::try_from(bytes.len()).unwrap_or(u32::MAX);
                context.registers[R5] = metadata.attributes;
                Ok(())
            }
            other => Err(RuntimeError::Program(format!(
                "OS_File reason {other} is not implemented by the hosted FileSwitch"
            ))),
        }
    }

    fn metadata_for_guest_object(
        &self,
        task: &Task,
        path: &str,
    ) -> Result<FileMetadata, RuntimeError> {
        let resolved = self
            .file_system
            .canonical_guest_path(&task.file_system, path)?;
        if resolved.is_directory {
            return Err(RuntimeError::Program(format!("'{path}' is a directory")));
        }
        Ok(resolved
            .metadata
            .unwrap_or_else(|| metadata_for_new_guest_path(&resolved.guest_path)))
    }

    fn catalogue_object(
        &self,
        task: &Task,
        path: &str,
    ) -> Result<(u32, FileMetadata, u32), RuntimeError> {
        let resolved = self
            .file_system
            .canonical_guest_path(&task.file_system, path)?;
        if !resolved.host_path.exists() {
            return Ok((0, metadata_for_new_guest_path(&resolved.guest_path), 0));
        }
        if resolved.is_directory {
            return Ok((
                2,
                FileMetadata {
                    guest_name: leaf_name(&resolved.guest_path).to_string(),
                    file_type: 0,
                    load_address: 0,
                    execution_address: 0,
                    attributes: 0,
                },
                0,
            ));
        }
        let metadata = resolved
            .metadata
            .unwrap_or_else(|| metadata_for_new_guest_path(&resolved.guest_path));
        let length =
            u32::try_from(std::fs::metadata(&resolved.host_path)?.len()).unwrap_or(u32::MAX);
        Ok((1, metadata, length))
    }

    fn os_find(&mut self, task: &mut Task, context: &mut SwiContext) -> Result<(), RuntimeError> {
        let reason = context.registers[R0] as u8;
        if reason == 0 {
            let handle = context.registers[R1];
            if handle == 0 {
                task.file_system.open_files.clear();
            } else {
                task.file_system.open_files.remove(&handle);
            }
            return Ok(());
        }

        let path = String::from_utf8_lossy(&mos::read_mos_string(
            task,
            context.registers[R1],
            MAX_STRING_BYTES,
        )?)
        .into_owned();
        let mode = reason & 0xC0;
        if !matches!(mode, 0x40 | 0x80 | 0xC0) {
            return Err(RuntimeError::Program(format!(
                "unsupported OS_Find reason &{reason:02X}"
            )));
        }
        let read = mode != 0x80;
        let write = mode != 0x40;
        let create = mode == 0x80;
        let truncate = create;
        let (file, resolved) = match self.file_system.open_file(
            &task.file_system,
            &path,
            read,
            write,
            create,
            truncate,
        ) {
            Ok(opened) => opened,
            Err(RuntimeError::Io(error))
                if error.kind() == std::io::ErrorKind::NotFound
                    && !create
                    && reason & 0x08 == 0 =>
            {
                context.registers[R0] = 0;
                return Ok(());
            }
            Err(error) => return Err(error),
        };
        let handle = task
            .file_system
            .insert_file(OpenFile {
                file,
                can_read: read,
                can_write: write,
                guest_path: resolved.guest_path,
                eof_error_next: false,
            })
            .ok_or_else(|| RuntimeError::Program("no FileSwitch handles are available".into()))?;
        context.registers[R0] = handle;
        Ok(())
    }

    fn os_args(&mut self, task: &mut Task, context: &mut SwiContext) -> Result<(), RuntimeError> {
        let reason = context.registers[R0];
        let handle = context.registers[R1];
        if reason == 0 && handle == 0 {
            context.registers[R0] = HOST_FS_NUMBER;
            return Ok(());
        }
        let file = task
            .file_system
            .open_files
            .get_mut(&handle)
            .ok_or_else(|| RuntimeError::Program(format!("invalid file handle {handle}")))?;
        match reason {
            0 => {
                context.registers[R2] =
                    u32::try_from(file.file.stream_position()?).unwrap_or(u32::MAX)
            }
            1 => {
                let position = u64::from(context.registers[R2]);
                let length = file.file.metadata()?.len();
                if position > length {
                    if !file.can_write {
                        return Err(RuntimeError::Program("file is not open for writing".into()));
                    }
                    file.file.set_len(position)?;
                }
                file.file.seek(SeekFrom::Start(position))?;
                file.eof_error_next = false;
            }
            2 | 4 => {
                context.registers[R2] =
                    u32::try_from(file.file.metadata()?.len()).unwrap_or(u32::MAX)
            }
            3 => {
                if !file.can_write {
                    return Err(RuntimeError::Program("file is not open for writing".into()));
                }
                let extent = u64::from(context.registers[R2]);
                file.file.set_len(extent)?;
                if file.file.stream_position()? > extent {
                    file.file.seek(SeekFrom::Start(extent))?;
                }
                file.eof_error_next = false;
            }
            5 => {
                context.registers[R2] =
                    u32::from(file.file.stream_position()? >= file.file.metadata()?.len());
            }
            7 => {
                let canonical =
                    canonical_guest_name(self.file_system.volume_name(), &file.guest_path);
                let bytes = canonical.as_bytes();
                let buffer = context.registers[R2];
                let capacity = context.registers[R5] as usize;
                let required = bytes.len() + 1;
                if required <= capacity {
                    task.memory.write_bytes(buffer, bytes)?;
                    task.memory.write_byte(buffer + bytes.len() as u32, 0)?;
                    context.registers[R5] = (capacity - required) as u32;
                } else {
                    context.registers[R5] = (capacity as i64 - required as i64) as i32 as u32;
                }
            }
            other => {
                return Err(RuntimeError::Program(format!(
                    "OS_Args reason {other} is not implemented by the hosted FileSwitch"
                )));
            }
        }
        Ok(())
    }

    fn os_bget(&mut self, task: &mut Task, context: &mut SwiContext) -> Result<(), RuntimeError> {
        let handle = context.registers[R1];
        let file = task
            .file_system
            .open_files
            .get_mut(&handle)
            .ok_or_else(|| RuntimeError::Program(format!("invalid file handle {handle}")))?;
        if !file.can_read {
            return Err(RuntimeError::Program("file is not open for reading".into()));
        }
        if file.eof_error_next {
            file.eof_error_next = false;
            return Err(RuntimeError::Program("end of file".into()));
        }
        let mut byte = [0u8; 1];
        if file.file.read(&mut byte)? == 0 {
            file.eof_error_next = true;
            context.carry = true;
        } else {
            context.registers[R0] = u32::from(byte[0]);
            context.carry = false;
        }
        Ok(())
    }

    fn os_bput(&mut self, task: &mut Task, context: &mut SwiContext) -> Result<(), RuntimeError> {
        let handle = context.registers[R1];
        let file = task
            .file_system
            .open_files
            .get_mut(&handle)
            .ok_or_else(|| RuntimeError::Program(format!("invalid file handle {handle}")))?;
        if !file.can_write {
            return Err(RuntimeError::Program("file is not open for writing".into()));
        }
        file.file.write_all(&[context.registers[R0] as u8])?;
        file.eof_error_next = false;
        Ok(())
    }

    fn os_gbpb(&mut self, task: &mut Task, context: &mut SwiContext) -> Result<(), RuntimeError> {
        match context.registers[R0] {
            1 | 2 | 3 | 4 => self.os_gbpb_file(task, context),
            5 | 6 | 7 => self.os_gbpb_names(task, context),
            8 | 9 | 10 => self.os_gbpb_directory(task, context),
            reason => Err(RuntimeError::Program(format!(
                "OS_GBPB reason {reason} is not implemented by the hosted FileSwitch"
            ))),
        }
    }

    fn os_gbpb_file(
        &mut self,
        task: &mut Task,
        context: &mut SwiContext,
    ) -> Result<(), RuntimeError> {
        let reason = context.registers[R0];
        let handle = context.registers[R1];
        let buffer = context.registers[R2];
        let requested = context.registers[R3] as usize;
        let file = task
            .file_system
            .open_files
            .get_mut(&handle)
            .ok_or_else(|| RuntimeError::Program(format!("invalid file handle {handle}")))?;
        if reason <= 2 {
            if !file.can_write {
                return Err(RuntimeError::Program("file is not open for writing".into()));
            }
            let initial = if reason == 1 {
                u64::from(context.registers[R4])
            } else {
                file.file.stream_position()?
            };
            file.file.seek(SeekFrom::Start(initial))?;
            let bytes = task.memory.read_bytes(buffer, requested)?;
            file.file.write_all(&bytes)?;
            let end = initial.saturating_add(requested as u64);
            context.registers[R2] = buffer.wrapping_add(requested as u32);
            context.registers[R3] = 0;
            context.registers[R4] = u32::try_from(end).unwrap_or(u32::MAX);
            context.carry = false;
            file.eof_error_next = false;
        } else {
            if !file.can_read {
                return Err(RuntimeError::Program("file is not open for reading".into()));
            }
            if requested > crate::memory::GUEST_MEMORY_SIZE {
                return Err(RuntimeError::Program(
                    "OS_GBPB transfer exceeds the task's logical buffer size".into(),
                ));
            }
            let initial = if reason == 3 {
                u64::from(context.registers[R4])
            } else {
                file.file.stream_position()?
            };
            file.file.seek(SeekFrom::Start(initial))?;
            let mut bytes = vec![0; requested];
            let transferred = file.file.read(&mut bytes)?;
            bytes.truncate(transferred);
            task.memory.write_bytes(buffer, &bytes)?;
            let end = initial.saturating_add(transferred as u64);
            context.registers[R2] = buffer.wrapping_add(transferred as u32);
            context.registers[R3] = u32::try_from(requested - transferred).unwrap_or(u32::MAX);
            context.registers[R4] = u32::try_from(end).unwrap_or(u32::MAX);
            context.carry = transferred != requested;
            file.eof_error_next = false;
        }
        Ok(())
    }

    fn os_gbpb_names(
        &mut self,
        task: &mut Task,
        context: &mut SwiContext,
    ) -> Result<(), RuntimeError> {
        let reason = context.registers[R0];
        let bytes = match reason {
            5 => {
                let name = self.file_system.volume_name().as_bytes();
                let mut bytes = vec![u8::try_from(name.len()).unwrap_or(u8::MAX)];
                bytes.extend_from_slice(name);
                bytes.push(0);
                bytes
            }
            6 => directory_name_bytes(&task.file_system.current_directory)?,
            _ => directory_name_bytes(&task.file_system.library_directory)?,
        };
        task.memory.write_bytes(context.registers[R2], &bytes)?;
        Ok(())
    }

    fn os_gbpb_directory(
        &mut self,
        task: &mut Task,
        context: &mut SwiContext,
    ) -> Result<(), RuntimeError> {
        let reason = context.registers[R0];
        let (directory, wildcard) = if reason == 8 {
            ("@".to_string(), "*".to_string())
        } else {
            let directory = read_guest_string(task, context.registers[R1])?;
            let wildcard = if context.registers[R6] == 0 {
                "*".to_string()
            } else {
                read_guest_string(task, context.registers[R6])?
            };
            (
                if directory.is_empty() {
                    "@".to_string()
                } else {
                    directory
                },
                wildcard,
            )
        };
        let objects = self
            .file_system
            .enumerate(&task.file_system, &directory, &wildcard)?;
        let start = context.registers[R4] as usize;
        let requested = context.registers[R3] as usize;
        if reason == 8 {
            let mut offset = context.registers[R2];
            let mut read_count = 0usize;
            for object in objects.iter().skip(start).take(requested) {
                let name = object.guest_name.as_bytes();
                if name.len() > u8::MAX as usize {
                    return Err(RuntimeError::Program("guest leaf name is too long".into()));
                }
                task.memory.write_byte(offset, name.len() as u8)?;
                offset = offset.wrapping_add(1);
                task.memory.write_bytes(offset, name)?;
                offset = offset.wrapping_add(name.len() as u32);
                read_count += 1;
            }
            context.registers[R3] = (requested - read_count) as u32;
            context.registers[R4] = if start + read_count >= objects.len() {
                u32::MAX
            } else {
                (start + read_count) as u32
            };
            context.carry = read_count < requested;
            return Ok(());
        }

        let mut offset = context.registers[R2];
        let mut capacity = context.registers[R5] as usize;
        let mut read_count = 0usize;
        for object in objects.iter().skip(start).take(requested) {
            let mut record = Vec::new();
            if reason == 9 {
                record.extend_from_slice(object.guest_name.as_bytes());
                record.push(0);
            } else {
                let metadata = &object.metadata;
                push_word(&mut record, riscos_load_address(metadata));
                push_word(&mut record, metadata.execution_address);
                push_word(&mut record, object.length);
                push_word(&mut record, metadata.attributes);
                push_word(&mut record, if object.is_directory { 2 } else { 1 });
                record.extend_from_slice(object.guest_name.as_bytes());
                record.push(0);
                while record.len() % 4 != 0 {
                    record.push(0);
                }
            }
            if record.len() > capacity {
                break;
            }
            task.memory.write_bytes(offset, &record)?;
            offset = offset.wrapping_add(record.len() as u32);
            capacity -= record.len();
            read_count += 1;
        }
        context.registers[R3] = read_count as u32;
        context.registers[R4] = if start + read_count >= objects.len() {
            u32::MAX
        } else {
            (start + read_count) as u32
        };
        context.carry = read_count != 0;
        Ok(())
    }

    fn os_fscontrol(
        &mut self,
        task: &mut Task,
        context: &mut SwiContext,
    ) -> Result<(), RuntimeError> {
        let reason = context.registers[R0];
        match reason {
            0 => {
                let path = read_guest_string(task, context.registers[R1])?;
                self.file_system
                    .set_current_directory(&mut task.file_system, &path)
            }
            1 => {
                let path = read_guest_string(task, context.registers[R1])?;
                self.file_system
                    .set_library_directory(&mut task.file_system, &path)
            }
            5 | 6 | 7 | 8 => {
                let path = if context.registers[R1] == 0 {
                    match reason {
                        7 | 8 => format!("%"),
                        _ => "@".to_string(),
                    }
                } else {
                    read_guest_string(task, context.registers[R1])?
                };
                self.catalogue_directory(task, &path, reason == 6 || reason == 8)
            }
            9 => {
                let pattern = if context.registers[R1] == 0 {
                    "*".to_string()
                } else {
                    read_guest_string(task, context.registers[R1])?
                };
                let (directory, wildcard) = pattern
                    .rsplit_once('.')
                    .map(|(directory, leaf)| (directory.to_string(), leaf.to_string()))
                    .unwrap_or_else(|| ("@".to_string(), pattern));
                self.catalogue_objects(task, &directory, &wildcard)
            }
            11 => {
                let prefix = if context.registers[R1] == 0 {
                    String::new()
                } else {
                    read_guest_string(task, context.registers[R1])?
                };
                if let Some((name, _)) = prefix.split_once(':') {
                    if !self.file_system.check_file_system_name(name) {
                        return Err(RuntimeError::Program(format!(
                            "filing system '{name}' is not present"
                        )));
                    }
                    task.file_system.temporary_file_system =
                        self.file_system.file_system_name().to_string();
                } else {
                    task.file_system.temporary_file_system =
                        task.file_system.current_file_system.clone();
                }
                Ok(())
            }
            19 => {
                task.file_system.temporary_file_system =
                    task.file_system.current_file_system.clone();
                Ok(())
            }
            13 => {
                let requested = context.registers[R1];
                let present = if requested < 0x100 {
                    requested == HOST_FS_NUMBER
                } else {
                    let name = read_guest_string(task, requested)?;
                    self.file_system
                        .check_file_system_name(name.trim_end_matches(['#', ':', '-']))
                };
                if present {
                    context.registers[R1] = HOST_FS_NUMBER;
                    context.registers[R2] = HOST_FS_CONTROL_BLOCK;
                } else {
                    context.registers[R2] = 0;
                }
                Ok(())
            }
            14 => {
                let requested = context.registers[R1];
                if requested == 0 {
                    task.file_system.current_file_system.clear();
                    task.file_system.temporary_file_system.clear();
                    return Ok(());
                }
                let name = if requested < 0x100 {
                    if requested != HOST_FS_NUMBER {
                        return Err(RuntimeError::Program(format!(
                            "filing system number {requested} is not present"
                        )));
                    }
                    self.file_system.file_system_name().to_string()
                } else {
                    read_guest_string(task, requested)?
                };
                if !self
                    .file_system
                    .check_file_system_name(name.trim_end_matches(['#', ':', '-']))
                {
                    return Err(RuntimeError::Program(format!(
                        "filing system '{name}' is not present"
                    )));
                }
                task.file_system.current_file_system =
                    self.file_system.file_system_name().to_string();
                task.file_system.temporary_file_system =
                    task.file_system.current_file_system.clone();
                Ok(())
            }
            18 => {
                let name = file_type_name(context.registers[R2] & 0xFFF);
                context.registers[R2] = u32::from_le_bytes(name[..4].try_into().unwrap());
                context.registers[R3] = u32::from_le_bytes(name[4..].try_into().unwrap());
                Ok(())
            }
            22 => {
                task.file_system.open_files.clear();
                Ok(())
            }
            25 => {
                let from = read_guest_string(task, context.registers[R1])?;
                let to = read_guest_string(task, context.registers[R2])?;
                self.file_system.rename(&task.file_system, &from, &to)
            }
            31 => {
                let name = read_guest_string(task, context.registers[R1])?;
                context.registers[R2] = parse_file_type(&name)?;
                Ok(())
            }
            33 => {
                if context.registers[R1] == HOST_FS_NUMBER {
                    write_guest_buffer(
                        task,
                        context.registers[R2],
                        context.registers[R3] as usize,
                        self.file_system.file_system_name().as_bytes(),
                    )?;
                } else {
                    task.memory.write_byte(context.registers[R2], 0)?;
                }
                Ok(())
            }
            37 => {
                let path = read_guest_string(task, context.registers[R1])?;
                let resolved = self
                    .file_system
                    .canonical_guest_path(&task.file_system, &path)?;
                let canonical =
                    canonical_guest_name(self.file_system.volume_name(), &resolved.guest_path);
                let capacity = context.registers[R5] as usize;
                let required = canonical.len() + 1;
                if required <= capacity {
                    task.memory
                        .write_bytes(context.registers[R2], canonical.as_bytes())?;
                    task.memory.write_byte(
                        context.registers[R2].wrapping_add(canonical.len() as u32),
                        0,
                    )?;
                    context.registers[R5] = (capacity - required) as u32;
                } else {
                    context.registers[R5] = (capacity as i64 - required as i64) as i32 as u32;
                }
                Ok(())
            }
            39 => {
                let path = read_guest_string(task, context.registers[R1])?;
                self.file_system.set_user_root(&mut task.file_system, &path)
            }
            40 => {
                std::mem::swap(
                    &mut task.file_system.current_directory,
                    &mut task.file_system.previous_directory,
                );
                Ok(())
            }
            43 => {
                task.file_system.current_directory.clear();
                Ok(())
            }
            44 => {
                task.file_system.user_root.clear();
                Ok(())
            }
            45 => {
                task.file_system.library_directory.clear();
                Ok(())
            }
            50 => {
                let object = read_guest_string(task, context.registers[R1])?;
                let resolved = self
                    .file_system
                    .canonical_guest_path(&task.file_system, &object)?;
                if !resolved.host_path.exists() {
                    return Err(RuntimeError::Program(format!(
                        "object '{object}' does not exist on HostFS"
                    )));
                }
                let name = read_guest_string(task, context.registers[R2])?;
                self.file_system.set_volume_name(name.trim())
            }
            other => Err(RuntimeError::Program(format!(
                "OS_FSControl reason {other} is not implemented by the hosted FileSwitch"
            ))),
        }
    }

    fn catalogue_directory(
        &mut self,
        task: &mut Task,
        path: &str,
        detailed: bool,
    ) -> Result<(), RuntimeError> {
        let resolved = self
            .file_system
            .canonical_guest_path(&task.file_system, path)?;
        if !resolved.is_directory {
            return Err(RuntimeError::Program(format!(
                "'{path}' is not a directory"
            )));
        }
        let objects = self.file_system.enumerate(&task.file_system, path, "*")?;
        let title = canonical_guest_name(self.file_system.volume_name(), &resolved.guest_path);
        self.write_inline(task, title.as_bytes())?;
        self.write_new_line(task)?;
        for object in objects {
            if detailed {
                let kind = if object.is_directory {
                    "<DIR>"
                } else {
                    "     "
                };
                let line = format!(
                    "{:<10} {:>8} {} {:03X}",
                    object.guest_name,
                    object.length,
                    kind,
                    object.metadata.file_type & 0xFFF
                );
                self.write_inline(task, line.as_bytes())?;
                self.write_new_line(task)?;
            } else {
                self.write_inline(task, object.guest_name.as_bytes())?;
                self.write_new_line(task)?;
            }
        }
        Ok(())
    }

    fn catalogue_objects(
        &mut self,
        task: &mut Task,
        directory: &str,
        wildcard: &str,
    ) -> Result<(), RuntimeError> {
        let resolved = self
            .file_system
            .canonical_guest_path(&task.file_system, directory)?;
        if !resolved.is_directory {
            return Err(RuntimeError::Program(format!(
                "'{directory}' is not a directory"
            )));
        }
        let objects = self
            .file_system
            .enumerate(&task.file_system, directory, wildcard)?;
        for object in objects {
            let kind = if object.is_directory {
                "<DIR>"
            } else {
                "     "
            };
            let line = format!(
                "{:<10} {:>8} {} {:03X}",
                object.guest_name,
                object.length,
                kind,
                object.metadata.file_type & 0xFFF
            );
            self.write_inline(task, line.as_bytes())?;
            self.write_new_line(task)?;
        }
        Ok(())
    }

    pub(crate) fn write_via_os_write_c(
        &mut self,
        task: &mut Task,
        bytes: &[u8],
    ) -> Result<(), RuntimeError> {
        for byte in bytes {
            self.emit_via_write_c(task, *byte)?;
        }
        Ok(())
    }

    pub(crate) fn plot(
        &mut self,
        task: &mut Task,
        plot_code: u8,
        x: i32,
        y: i32,
    ) -> Result<(), RuntimeError> {
        let mut context = SwiContext::default();
        context.registers[R0] = u32::from(plot_code);
        context.registers[R1] = x as u32;
        context.registers[R2] = y as u32;
        self.dispatch(OS_PLOT, task, &mut context)
    }

    pub fn flush(&mut self) -> Result<(), RuntimeError> {
        self.console.flush().map_err(RuntimeError::from)
    }

    pub fn publish_display_event(&self, event: DisplayEvent) {
        if let Some(sender) = &self.display_events {
            let _ = sender.send(event);
        }
    }

    pub(crate) fn begin_display_batch(&mut self) {
        self.display_batch_active = true;
        self.last_display_batch_publish = Some(Instant::now());
    }

    pub(crate) fn finish_display_batch(&mut self) {
        if !self.display_batch_active {
            return;
        }
        self.display_batch_active = false;
        self.last_display_batch_publish = None;
        if self.display_events.is_some() {
            self.publish_snapshot(self.current_graphics().snapshot().clone());
        }
    }

    fn publish_display_batch_snapshot_if_due(&mut self) {
        if self.display_events.is_none()
            || !self
                .last_display_batch_publish
                .is_some_and(|last| last.elapsed() >= DISPLAY_BATCH_FRAME_INTERVAL)
        {
            return;
        }

        let snapshot = self.current_graphics().snapshot().clone();
        self.publish_snapshot(snapshot);
        self.last_display_batch_publish = Some(Instant::now());
    }

    pub fn write_inline(&mut self, task: &mut Task, text: &[u8]) -> Result<(), RuntimeError> {
        self.write_string_to_output_buffer(task, text)?;

        let mut context = SwiContext {
            pc: OUTPUT_BUFFER,
            ..SwiContext::default()
        };
        self.dispatch(OS_WRITE_S, task, &mut context)
    }

    pub fn write_indirect(&mut self, task: &mut Task, text: &[u8]) -> Result<(), RuntimeError> {
        self.write_string_to_output_buffer(task, text)?;
        let mut context = SwiContext::default();
        context.registers[R0] = OUTPUT_BUFFER;
        self.dispatch(OS_WRITE_0, task, &mut context)
    }

    fn write_string_to_output_buffer(
        &mut self,
        task: &mut Task,
        text: &[u8],
    ) -> Result<u32, RuntimeError> {
        task.memory.write_bytes(OUTPUT_BUFFER, text)?;
        let terminator =
            OUTPUT_BUFFER
                .checked_add(u32::try_from(text.len()).map_err(|_| {
                    RuntimeError::Memory(crate::memory::MemoryError::AddressOverflow)
                })?)
                .ok_or(crate::memory::MemoryError::AddressOverflow)?;
        task.memory.write_byte(terminator, 0)?;
        Ok(terminator)
    }

    fn emit_via_write_c(&mut self, task: &mut Task, byte: u8) -> Result<(), RuntimeError> {
        let mut context = SwiContext::default();
        context.registers[R0] = u32::from(byte);
        self.dispatch(OS_WRITE_C, task, &mut context)
    }

    fn write_inline_string(
        &mut self,
        task: &mut Task,
        context: &mut SwiContext,
    ) -> Result<(), RuntimeError> {
        let mut address = context.pc;
        for _ in 0..MAX_STRING_BYTES {
            let byte = task.memory.read_byte(address)?;
            address = address
                .checked_add(1)
                .ok_or(crate::memory::MemoryError::AddressOverflow)?;
            if byte == 0 {
                let after_string = address
                    .checked_add(3)
                    .ok_or(crate::memory::MemoryError::AddressOverflow)?;
                context.pc = after_string & !3;
                return Ok(());
            }
            self.emit_via_write_c(task, byte)?;
        }
        Err(crate::memory::MemoryError::MissingNullTerminator(context.pc).into())
    }

    fn write_indirect_string(
        &mut self,
        task: &mut Task,
        context: &mut SwiContext,
    ) -> Result<(), RuntimeError> {
        let start = context.registers[R0];
        let bytes = task.memory.read_c_string(start, MAX_STRING_BYTES)?;
        for byte in &bytes {
            self.emit_via_write_c(task, *byte)?;
        }
        context.registers[R0] =
            start
                .checked_add(u32::try_from(bytes.len()).map_err(|_| {
                    RuntimeError::Memory(crate::memory::MemoryError::AddressOverflow)
                })?)
                .and_then(|address| address.checked_add(1))
                .ok_or(crate::memory::MemoryError::AddressOverflow)?;
        Ok(())
    }

    fn read_character(&mut self, context: &mut SwiContext) -> Result<(), RuntimeError> {
        let byte = match self.mos.input.pop_front() {
            Some(byte) => byte,
            None => self.console.read_byte()?.ok_or(RuntimeError::EndOfInput)?,
        };
        context.registers[R0] = u32::from(byte);
        context.carry = byte == 0x1B;
        Ok(())
    }

    fn read_line(&mut self, task: &mut Task, context: &mut SwiContext) -> Result<(), RuntimeError> {
        let buffer = context.registers[R0] & GUEST_ADDRESS_MASK;
        let maximum = context.registers[R1] as usize;
        let lowest = context.registers[R2].min(255) as u8;
        let highest = context.registers[R3].min(255) as u8;
        let echo_only_buffered = context.registers[R0] & READ_LINE_ECHO_ONLY_BUFFERED != 0;
        let echo_r4 = context.registers[R0] & READ_LINE_ECHO_R4 != 0;
        let echo_byte = context.registers[R4] as u8;
        let software_echo = self.console.software_echo();
        let mut length = 0_usize;
        context.carry = false;

        loop {
            let mut character = SwiContext::default();
            match self.dispatch(OS_READ_C, task, &mut character) {
                Ok(()) => {}
                Err(RuntimeError::EndOfInput) if length > 0 => break,
                Err(error) => return Err(error),
            }
            let byte = character.registers[R0] as u8;

            if byte == b'\r' || byte == b'\n' {
                if software_echo {
                    self.write_new_line(task)?;
                }
                break;
            }
            if byte == 0x1B {
                context.carry = true;
                if software_echo {
                    self.write_new_line(task)?;
                }
                break;
            }
            if byte == 0x04 && length == 0 {
                return Err(RuntimeError::EndOfInput);
            }
            if byte == 0x08 || byte == 0x7F {
                if length > 0 {
                    length -= 1;
                    task.memory.write_byte(
                        buffer
                            .checked_add(
                                u32::try_from(length)
                                    .map_err(|_| crate::memory::MemoryError::AddressOverflow)?,
                            )
                            .ok_or(crate::memory::MemoryError::AddressOverflow)?,
                        0,
                    )?;
                    if software_echo {
                        self.emit_via_write_c(task, 0x08)?;
                        self.emit_via_write_c(task, b' ')?;
                        self.emit_via_write_c(task, 0x08)?;
                    }
                }
                continue;
            }

            let accepted = byte >= lowest && byte <= highest;
            let buffer_full = accepted && length >= maximum;
            if accepted && length < maximum {
                let address = buffer
                    .checked_add(
                        u32::try_from(length)
                            .map_err(|_| crate::memory::MemoryError::AddressOverflow)?,
                    )
                    .ok_or(crate::memory::MemoryError::AddressOverflow)?;
                task.memory.write_byte(address, byte)?;
                length += 1;
                if software_echo {
                    self.emit_via_write_c(task, if echo_r4 { echo_byte } else { byte })?;
                }
            } else if software_echo && !echo_only_buffered {
                self.emit_via_write_c(task, if echo_r4 { echo_byte } else { byte })?;
            }

            if buffer_full && software_echo {
                self.emit_via_write_c(task, 0x07)?;
            }
        }

        context.registers[R0] = 0;
        context.registers[R1] =
            u32::try_from(length).map_err(|_| crate::memory::MemoryError::AddressOverflow)?;
        context.registers[R2] = 0;
        context.registers[R3] = 0;
        Ok(())
    }

    fn execute_cli(
        &mut self,
        task: &mut Task,
        context: &mut SwiContext,
    ) -> Result<(), RuntimeError> {
        let bytes = mos::read_mos_string(task, context.registers[R0], MAX_CLI_BYTES)?;
        let command = String::from_utf8_lossy(&bytes);
        let command = command.trim();
        let command = command.trim_start_matches('*').trim_start();
        if command.is_empty() {
            return Ok(());
        }

        let compact_fx = command
            .get(..2)
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case("FX"))
            && command
                .as_bytes()
                .get(2)
                .is_some_and(|next| !next.is_ascii_whitespace());
        let (verb, arguments) = if compact_fx {
            (&command[..2], command[2..].trim())
        } else {
            let mut words = command.splitn(2, char::is_whitespace);
            (
                words.next().unwrap_or_default(),
                words.next().unwrap_or_default().trim(),
            )
        };

        if cli_command_matches(verb, "HELP") && arguments.is_empty() {
            self.write_inline(task, HELP_TEXT)?;
            self.write_new_line(task)
        } else if cli_command_matches(verb, "QUIT") && arguments.is_empty() {
            self.quit_requested = true;
            Ok(())
        } else if cli_command_matches(verb, "CONFIGURE") {
            self.execute_configure_command(task, arguments)
        } else if cli_command_matches(verb, "STATUS") {
            self.execute_status_command(task, arguments)
        } else if cli_command_matches(verb, "BASIC") {
            if arguments.is_empty() {
                self.write_inline(task, b"Syntax: *BASIC <file>")?;
                return self.write_new_line(task);
            }

            let configuration = match self.load_basic_configuration() {
                Ok(configuration) => configuration,
                Err(error) => {
                    let message = format!("BASIC configuration error: {error}");
                    self.write_inline(task, message.as_bytes())?;
                    return self.write_new_line(task);
                }
            };
            let path = unquote_single_argument(arguments);
            self.begin_display_batch();
            let result =
                crate::basic64::run_guest_file_configured(path, task, self, &configuration);
            self.finish_display_batch();
            match result {
                Ok(Some(report)) => log_jit_report("BASIC", report),
                Ok(None) => {}
                Err(error) => {
                    let message = format!("BASIC error: {error}");
                    self.write_inline(task, message.as_bytes())?;
                    self.write_new_line(task)?;
                }
            }
            Ok(())
        } else if cli_command_matches(verb, "RUN") {
            if arguments.is_empty() {
                self.write_inline(task, b"Syntax: RUN <file.bas64|bas|txt|asc|bbc>")?;
                return self.write_new_line(task);
            }

            let configuration = match self.load_basic_configuration() {
                Ok(configuration) => configuration,
                Err(error) => {
                    let message = format!("BASIC configuration error: {error}");
                    self.write_inline(task, message.as_bytes())?;
                    return self.write_new_line(task);
                }
            };
            let path = unquote_single_argument(arguments);
            self.begin_display_batch();
            let result =
                crate::basic64::run_guest_file_configured(path, task, self, &configuration);
            self.finish_display_batch();
            match result {
                Ok(Some(report)) => {
                    log_jit_report("RUN", report);
                    Ok(())
                }
                Ok(None) => Ok(()),
                Err(error) => {
                    let message = format!("BASIC error: {error}");
                    self.write_inline(task, message.as_bytes())?;
                    self.write_new_line(task)
                }
            }
        } else if cli_command_matches(verb, "BASICLOAD") {
            if arguments.is_empty() {
                self.write_inline(task, b"Syntax: BASICLOAD <file>")?;
                return self.write_new_line(task);
            }

            let path = unquote_single_argument(arguments);
            match self.file_system.read_file(&task.file_system, path) {
                Ok((bytes, metadata)) if metadata.file_type & 0xFFF == FILETYPE_BASIC => {
                    let program =
                        match crate::tokenized_basic::TokenizedBasicProgram::decode(&bytes) {
                            Ok(program) => program,
                            Err(error) => {
                                let message = format!("BASICLOAD error: {error}");
                                self.write_inline(task, message.as_bytes())?;
                                return self.write_new_line(task);
                            }
                        };
                    let line_count = program.line_count();
                    let reference_count = program.line_reference_count();
                    let unresolved_count = program.unresolved_line_reference_count();
                    task.loaded_tokenized_program = Some(program);
                    let message = format!(
                        "Loaded {line_count} tokenised BASIC lines; {reference_count} line references, {unresolved_count} unresolved."
                    );
                    self.write_inline(task, message.as_bytes())?;
                    self.write_new_line(task)
                }
                Ok((_, metadata)) => {
                    let message = format!(
                        "BASICLOAD requires file type &FFB; '{}' has type &{:03X}.",
                        path,
                        metadata.file_type & 0xFFF
                    );
                    self.write_inline(task, message.as_bytes())?;
                    self.write_new_line(task)
                }
                Err(error) => {
                    let message = format!("BASICLOAD error: {error}");
                    self.write_inline(task, message.as_bytes())?;
                    self.write_new_line(task)
                }
            }
        } else if cli_command_matches(verb, "BASICRUN") {
            if !arguments.is_empty() {
                self.write_inline(task, b"Syntax: BASICRUN")?;
                return self.write_new_line(task);
            }

            let Some(program) = task.loaded_tokenized_program.take() else {
                self.write_inline(
                    task,
                    b"No tokenised BASIC program is loaded; use BASICLOAD first.",
                )?;
                return self.write_new_line(task);
            };
            self.begin_display_batch();
            let configuration = match self.load_basic_configuration() {
                Ok(configuration) => configuration,
                Err(error) => {
                    self.finish_display_batch();
                    task.loaded_tokenized_program = Some(program);
                    let message = format!("BASIC configuration error: {error}");
                    self.write_inline(task, message.as_bytes())?;
                    return self.write_new_line(task);
                }
            };
            let result =
                crate::basic_compat::run_program_configured(&program, task, self, &configuration);
            self.finish_display_batch();
            task.loaded_tokenized_program = Some(program);
            match result {
                Ok(Some(report)) => {
                    log_jit_report("BASICRUN", report);
                    Ok(())
                }
                Ok(None) => Ok(()),
                Err(error) => {
                    let message = format!("BASICRUN error: {error}");
                    self.write_inline(task, message.as_bytes())?;
                    self.write_new_line(task)
                }
            }
        } else if cli_command_matches(verb, "BASICJIT") {
            self.begin_display_batch();
            let arguments = arguments.trim();
            let (strict, arguments) = match arguments.split_once(char::is_whitespace) {
                Some((first, rest)) if first.eq_ignore_ascii_case("STRICT") => (true, rest.trim()),
                None if arguments.eq_ignore_ascii_case("STRICT") => (true, ""),
                _ => (false, arguments),
            };
            let (benchmark_validation, arguments) = if strict {
                match arguments.strip_prefix("--benchmark-validation") {
                    Some(rest) if rest.is_empty() || rest.starts_with(char::is_whitespace) => {
                        (true, rest.trim())
                    }
                    _ => (false, arguments),
                }
            } else {
                (false, arguments)
            };
            let options = crate::basic_compat::StrictJitOptions {
                benchmark_validation,
            };
            let configuration = match self.load_basic_configuration() {
                Ok(configuration) => configuration,
                Err(error) => {
                    self.finish_display_batch();
                    eprintln!("BASICJIT configuration error: {error}");
                    return Ok(());
                }
            };
            let engine = if strict {
                BasicEngine::StrictJit
            } else {
                BasicEngine::HybridJit
            };
            let result = if arguments.is_empty() {
                let Some(program) = task.loaded_tokenized_program.take() else {
                    self.finish_display_batch();
                    self.write_inline(
                        task,
                        b"No tokenised BASIC program is loaded; use BASICLOAD or pass a file.",
                    )?;
                    return self.write_new_line(task);
                };
                let result = crate::basic_compat::run_program_with_engine_options(
                    &program,
                    task,
                    self,
                    &configuration,
                    Some(engine),
                    options,
                );
                task.loaded_tokenized_program = Some(program);
                result
            } else {
                let path = unquote_single_argument(arguments);
                crate::basic64::run_guest_file_with_engine_options(
                    path,
                    task,
                    self,
                    &configuration,
                    Some(engine),
                    options,
                )
            };
            self.finish_display_batch();
            match result {
                Ok(Some(report)) => {
                    log_jit_report("BASICJIT", report);
                    Ok(())
                }
                Ok(None) => Ok(()),
                Err(error) => {
                    eprintln!("BASICJIT error: {error}");
                    Ok(())
                }
            }
        } else if verb == "." || cli_command_matches(verb, "CAT") {
            let path = unquote_single_argument(arguments);
            let mut call = SwiContext::default();
            call.registers[R0] = 5;
            if !path.is_empty() {
                call.registers[R1] = CLI_STRING_BUFFER;
                write_guest_string(task, CLI_STRING_BUFFER, path)?;
            }
            self.dispatch(OS_FSCONTROL, task, &mut call)
        } else if cli_command_matches(verb, "DIR") {
            let path = unquote_single_argument(arguments);
            if path.is_empty() {
                let selected = canonical_guest_name(
                    self.file_system.volume_name(),
                    &task.file_system.current_directory.join("."),
                );
                self.write_inline(task, selected.as_bytes())?;
                self.write_new_line(task)
            } else {
                write_guest_string(task, CLI_STRING_BUFFER, path)?;
                let mut call = SwiContext::default();
                call.registers[R0] = 0;
                call.registers[R1] = CLI_STRING_BUFFER;
                self.dispatch(OS_FSCONTROL, task, &mut call)
            }
        } else if cli_command_matches(verb, "CDIR") {
            let path = unquote_single_argument(arguments);
            if path.is_empty() {
                self.write_inline(task, b"Syntax: *CDIR <directory>")?;
                return self.write_new_line(task);
            }
            write_guest_string(task, CLI_STRING_BUFFER, path)?;
            let mut call = SwiContext::default();
            call.registers[R0] = 8;
            call.registers[R1] = CLI_STRING_BUFFER;
            self.dispatch(OS_FILE, task, &mut call)
        } else if cli_command_matches(verb, "DELETE") {
            let path = unquote_single_argument(arguments);
            if path.is_empty() {
                self.write_inline(task, b"Syntax: *DELETE <file>")?;
                return self.write_new_line(task);
            }
            write_guest_string(task, CLI_STRING_BUFFER, path)?;
            let mut call = SwiContext::default();
            call.registers[R0] = 6;
            call.registers[R1] = CLI_STRING_BUFFER;
            self.dispatch(OS_FILE, task, &mut call)
        } else if cli_command_matches(verb, "RENAME") {
            let Some((from, to)) = split_two_cli_arguments(arguments) else {
                self.write_inline(task, b"Syntax: *RENAME <old> <new>")?;
                return self.write_new_line(task);
            };
            write_guest_string(task, CLI_STRING_BUFFER, &from)?;
            write_guest_string(task, CLI_STRING_BUFFER + 0x1000, &to)?;
            let mut call = SwiContext::default();
            call.registers[R0] = 25;
            call.registers[R1] = CLI_STRING_BUFFER;
            call.registers[R2] = CLI_STRING_BUFFER + 0x1000;
            self.dispatch(OS_FSCONTROL, task, &mut call)
        } else if cli_command_matches(verb, "FILETYPE") {
            let Some((path, type_name)) = split_two_cli_arguments(arguments) else {
                self.write_inline(task, b"Syntax: *FILETYPE <file> <type>")?;
                return self.write_new_line(task);
            };
            write_guest_string(task, CLI_STRING_BUFFER, &path)?;
            write_guest_string(task, CLI_STRING_BUFFER + 0x1000, &type_name)?;
            let mut convert = SwiContext::default();
            convert.registers[R0] = 31;
            convert.registers[R1] = CLI_STRING_BUFFER + 0x1000;
            self.dispatch(OS_FSCONTROL, task, &mut convert)?;
            let mut set_type = SwiContext::default();
            set_type.registers[R0] = 18;
            set_type.registers[R1] = CLI_STRING_BUFFER;
            set_type.registers[R2] = convert.registers[R2];
            self.dispatch(OS_FILE, task, &mut set_type)
        } else if cli_command_matches(verb, "TYPE") {
            let path = unquote_single_argument(arguments);
            if path.is_empty() {
                self.write_inline(task, b"Syntax: *TYPE <file>")?;
                return self.write_new_line(task);
            }
            write_guest_string(task, CLI_STRING_BUFFER, path)?;
            let mut open = SwiContext::default();
            open.registers[R0] = 0x40;
            open.registers[R1] = CLI_STRING_BUFFER;
            self.dispatch(OS_FIND, task, &mut open)?;
            if open.registers[R0] == 0 {
                self.write_inline(task, b"File not found")?;
                return self.write_new_line(task);
            }
            let handle = open.registers[R0];
            let read_result = loop {
                let mut read = SwiContext::default();
                read.registers[R1] = handle;
                if let Err(error) = self.dispatch(OS_BGET, task, &mut read) {
                    break Err(error);
                }
                if read.carry {
                    break Ok(());
                }
                let mut output = SwiContext::default();
                output.registers[R0] = read.registers[R0];
                if let Err(error) = self.dispatch(OS_WRITE_C, task, &mut output) {
                    break Err(error);
                }
            };
            let mut close = SwiContext::default();
            close.registers[R0] = 0;
            close.registers[R1] = handle;
            self.dispatch(OS_FIND, task, &mut close)?;
            read_result
        } else if cli_command_matches(verb, "DISC") {
            if arguments.is_empty() {
                let mut call = SwiContext::default();
                call.registers[R0] = 5;
                call.registers[R2] = CLI_STRING_BUFFER;
                self.dispatch(OS_GBPB, task, &mut call)?;
                let length = task.memory.read_byte(CLI_STRING_BUFFER)? as usize;
                let name = task.memory.read_bytes(CLI_STRING_BUFFER + 1, length)?;
                self.write_inline(task, &name)?;
                self.write_new_line(task)
            } else {
                write_guest_string(task, CLI_STRING_BUFFER + 0x1000, arguments.trim())?;
                write_guest_string(task, CLI_STRING_BUFFER, "@")?;
                let mut call = SwiContext::default();
                call.registers[R0] = 50;
                call.registers[R1] = CLI_STRING_BUFFER;
                call.registers[R2] = CLI_STRING_BUFFER + 0x1000;
                self.dispatch(OS_FSCONTROL, task, &mut call)
            }
        } else if cli_command_matches(verb, "HOSTFS") {
            write_guest_string(task, CLI_STRING_BUFFER, "HostFS")?;
            let mut call = SwiContext::default();
            call.registers[R0] = 14;
            call.registers[R1] = CLI_STRING_BUFFER;
            self.dispatch(OS_FSCONTROL, task, &mut call)
        } else if cli_command_matches(verb, "DESKTOP") {
            if !arguments.is_empty() {
                self.write_inline(task, b"Syntax: DESKTOP")?;
                return self.write_new_line(task);
            }
            let Some(wimp) = self.desktop_service.as_ref().cloned() else {
                return Err(RuntimeError::Program(
                    "DESKTOP requires the windowed host; restart without --stdio".into(),
                ));
            };

            // Validate the saved preference at the desktop boundary, then
            // make the same store available to BASIC tasks started by Wimp.
            self.load_basic_configuration()?;
            self.desktop_service = None;
            wimp.set_configure_store(self.configure.clone());

            self.desktop_requested = true;
            self.wimp = Some(Arc::clone(&wimp));
            self.publish_display_event(DisplayEvent::DesktopStarted);
            // Keep the MOS/BASIC caller suspended at the command boundary while
            // the shared Wimp desktop owns the hosted display.
            wimp.wait_until_stopped()?;
            Ok(())
        } else if cli_command_matches(verb, "FX")
            && arguments
                .chars()
                .filter(|character| !character.is_ascii_whitespace())
                .collect::<String>()
                == "151,78,243"
        {
            // ClockSP5 resets machine-specific display and timing state here;
            // the hosted profile has no such hardware state to restore.
            Ok(())
        } else {
            self.write_inline(task, b"Bad command")?;
            self.write_new_line(task)
        }
    }

    fn execute_configure_command(
        &mut self,
        task: &mut Task,
        arguments: &str,
    ) -> Result<(), RuntimeError> {
        let arguments = arguments.trim();
        if arguments.is_empty() {
            self.write_inline(
                task,
                b"Syntax: *CONFIGURE <option> <value>\n\r  Language 0|3 (MOS prompt|desktop on load)\n\r  WindowFurniture Flat|Bevelled (restart app to apply)\n\r  BASICMode Auto|Classic|BASIC64|Hybrid\n\r  BASICProfile Auto|<profile>\n\r  BASICTarget Auto|Hosted|RISCOS|Agon\n\r  BASICEngine Interpreter|Hybrid|Strict\n\r  *CONFIGURE DEFAULTS resets all configuration preferences.",
            )?;
            return self.write_new_line(task);
        }
        if arguments.eq_ignore_ascii_case("DEFAULTS") {
            return match self.effective_configure_store().reset() {
                Ok(_) => {
                    self.write_inline(task, b"Configuration preferences restored to defaults.")?;
                    self.write_new_line(task)
                }
                Err(error) => {
                    let message = format!("CONFIGURE error: {error}");
                    self.write_inline(task, message.as_bytes())?;
                    self.write_new_line(task)
                }
            };
        }

        let mut words = arguments.split_ascii_whitespace();
        let Some(option) = words.next() else {
            return self.write_new_line(task);
        };
        let Some(value) = words.next() else {
            self.write_inline(task, b"Syntax: *CONFIGURE <option> <value>")?;
            return self.write_new_line(task);
        };
        if words.next().is_some() {
            self.write_inline(task, b"Syntax: *CONFIGURE <option> <value>")?;
            return self.write_new_line(task);
        }

        match self.effective_configure_store().set(option, value) {
            Ok(configuration) => {
                let (canonical, value) = configuration
                    .status_value(option)
                    .expect("validated CONFIGURE option");
                let message = format!("{canonical} set to {value}.");
                self.write_inline(task, message.as_bytes())?;
                self.write_new_line(task)
            }
            Err(error) => {
                let message = format!("CONFIGURE error: {error}");
                self.write_inline(task, message.as_bytes())?;
                self.write_new_line(task)
            }
        }
    }

    fn execute_status_command(
        &mut self,
        task: &mut Task,
        arguments: &str,
    ) -> Result<(), RuntimeError> {
        let arguments = arguments.trim();
        if arguments.split_ascii_whitespace().nth(1).is_some() {
            self.write_inline(task, b"Syntax: *STATUS [option]")?;
            return self.write_new_line(task);
        }
        let configuration = match self.load_basic_configuration() {
            Ok(configuration) => configuration,
            Err(error) => {
                let message = format!("STATUS error: {error}");
                self.write_inline(task, message.as_bytes())?;
                return self.write_new_line(task);
            }
        };
        if arguments.is_empty() {
            let mut output = String::new();
            for (option, value) in configuration.status_entries() {
                output.push_str(option);
                output.push('=');
                output.push_str(&value);
                output.push_str("\n\r");
            }
            return self.write_inline(task, output.as_bytes());
        }
        match configuration.status_value(arguments) {
            Some((option, value)) => {
                let output = format!("{option}={value}");
                self.write_inline(task, output.as_bytes())?;
                self.write_new_line(task)
            }
            None => {
                let message = format!("STATUS error: unknown configuration option '{arguments}'");
                self.write_inline(task, message.as_bytes())?;
                self.write_new_line(task)
            }
        }
    }

    fn write_new_line(&mut self, task: &mut Task) -> Result<(), RuntimeError> {
        self.dispatch(OS_NEW_LINE, task, &mut SwiContext::default())
    }
}

impl Drop for SwiDispatcher {
    fn drop(&mut self) {
        if let Some(wimp) = &self.wimp {
            wimp.task_exited(self.display_task_id);
        }
    }
}

fn cli_command_matches(command_token: &str, command_name: &str) -> bool {
    let Some(prefix) = command_token.strip_suffix('.') else {
        return command_token.eq_ignore_ascii_case(command_name);
    };

    !prefix.is_empty()
        && command_name
            .get(..prefix.len())
            .is_some_and(|leading| leading.eq_ignore_ascii_case(prefix))
}

fn read_guest_string(task: &Task, address: u32) -> Result<String, RuntimeError> {
    let bytes = task.memory.read_c_string(address, MAX_STRING_BYTES)?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

fn read_guest_control_string(task: &Task, address: u32) -> Result<String, RuntimeError> {
    let mut bytes = Vec::new();
    for offset in 0..MAX_STRING_BYTES {
        let current = address
            .checked_add(
                u32::try_from(offset).map_err(|_| crate::memory::MemoryError::AddressOverflow)?,
            )
            .ok_or(crate::memory::MemoryError::AddressOverflow)?;
        let byte = task.memory.read_byte(current)?;
        if byte <= 31 {
            return Ok(String::from_utf8_lossy(&bytes).into_owned());
        }
        bytes.push(byte);
    }
    Err(RuntimeError::Program(
        "guest control-terminated string is too long".into(),
    ))
}

fn write_guest_string(task: &mut Task, address: u32, value: &str) -> Result<(), RuntimeError> {
    if value.len() + 1 > MAX_STRING_BYTES {
        return Err(RuntimeError::Program("guest string is too long".into()));
    }
    task.memory.write_bytes(address, value.as_bytes())?;
    task.memory
        .write_byte(address.wrapping_add(value.len() as u32), 0)?;
    Ok(())
}

fn write_guest_buffer(
    task: &mut Task,
    address: u32,
    capacity: usize,
    bytes: &[u8],
) -> Result<(), RuntimeError> {
    if capacity == 0 {
        return Ok(());
    }
    let length = bytes.len().min(capacity - 1);
    task.memory.write_bytes(address, &bytes[..length])?;
    task.memory
        .write_byte(address.wrapping_add(length as u32), 0)?;
    Ok(())
}

fn metadata_for_new_guest_path(path: &str) -> FileMetadata {
    let path = path
        .rsplit(':')
        .next()
        .unwrap_or(path)
        .trim_end_matches('.');
    let guest_name = path.rsplit('.').next().unwrap_or(path);
    FileMetadata {
        guest_name: guest_name.to_string(),
        file_type: FILETYPE_TEXT,
        load_address: 0,
        execution_address: 0,
        attributes: 0,
    }
}

fn leaf_name(path: &str) -> &str {
    path.rsplit('.').next().unwrap_or(path)
}

fn riscos_load_address(metadata: &FileMetadata) -> u32 {
    ((metadata.file_type & 0xFFF) << 20) | (metadata.load_address & 0x000F_FFFF)
}

fn apply_riscos_load_address(metadata: &mut FileMetadata, load_address: u32) {
    let encoded_file_type = load_address >> 20;
    if encoded_file_type != 0 {
        metadata.file_type = encoded_file_type & 0xFFF;
        metadata.load_address = load_address & 0x000F_FFFF;
    } else {
        metadata.load_address = load_address;
    }
}

fn canonical_guest_name(volume_name: &str, guest_path: &str) -> String {
    if guest_path.is_empty() {
        format!("HostFS::{volume_name}.$")
    } else {
        format!("HostFS::{volume_name}.$.{guest_path}")
    }
}

fn directory_name_bytes(components: &[String]) -> Result<Vec<u8>, RuntimeError> {
    let directory = if components.is_empty() {
        "$".to_string()
    } else {
        format!("$.{}", components.join("."))
    };
    let bytes = directory.as_bytes();
    let length = u8::try_from(bytes.len())
        .map_err(|_| RuntimeError::Program("directory name is too long".into()))?;
    let mut result = vec![0, length];
    result.extend_from_slice(bytes);
    result.push(0);
    Ok(result)
}

fn push_word(bytes: &mut Vec<u8>, word: u32) {
    bytes.extend_from_slice(&word.to_le_bytes());
}

fn file_type_name(file_type: u32) -> [u8; 8] {
    let numeric;
    let name = match file_type & 0xFFF {
        0xFFB => "BASIC",
        FILETYPE_BASIC64 => "BASIC64",
        0xFFF => "Text",
        0xFEB => "Obey",
        0xFFD => "Data",
        value => {
            numeric = format!("{value:03X}");
            numeric.as_str()
        }
    };
    let mut bytes = [b' '; 8];
    let length = name.len().min(bytes.len());
    bytes[..length].copy_from_slice(&name.as_bytes()[..length]);
    bytes
}

fn parse_file_type(value: &str) -> Result<u32, RuntimeError> {
    let value = value.trim();
    let named = if value.eq_ignore_ascii_case("BASIC") {
        Some(FILETYPE_BASIC)
    } else if value.eq_ignore_ascii_case("BASIC64") {
        Some(FILETYPE_BASIC64)
    } else if value.eq_ignore_ascii_case("TEXT") {
        Some(FILETYPE_TEXT)
    } else if value.eq_ignore_ascii_case("OBEY") {
        Some(0xFEB)
    } else if value.eq_ignore_ascii_case("DATA") {
        Some(0xFFD)
    } else {
        None
    };
    if let Some(file_type) = named {
        return Ok(file_type);
    }
    let (radix, digits) = if let Some((radix, digits)) = value.split_once('_') {
        let radix = radix
            .parse::<u32>()
            .map_err(|_| RuntimeError::Program(format!("invalid file type '{value}'")))?;
        (radix, digits)
    } else {
        (
            16,
            value
                .strip_prefix("&")
                .or_else(|| value.strip_prefix("0x"))
                .unwrap_or(value),
        )
    };
    if !(2..=36).contains(&radix) {
        return Err(RuntimeError::Program(format!(
            "invalid file type radix {radix}"
        )));
    }
    let file_type = u32::from_str_radix(digits, radix)
        .map_err(|_| RuntimeError::Program(format!("invalid file type '{value}'")))?;
    if file_type > 0xFFF {
        return Err(RuntimeError::Program(format!(
            "RISC OS file type '{value}' must fit in 12 bits"
        )));
    }
    Ok(file_type)
}

fn unquote_single_argument(arguments: &str) -> &str {
    arguments
        .strip_prefix('"')
        .and_then(|path| path.strip_suffix('"'))
        .unwrap_or(arguments)
}

fn split_two_cli_arguments(arguments: &str) -> Option<(String, String)> {
    let arguments = arguments.trim();
    let (first, remainder) = if let Some(quoted) = arguments.strip_prefix('"') {
        let end = quoted.find('"')?;
        (&quoted[..end], quoted[end + 1..].trim())
    } else {
        let split = arguments.find(char::is_whitespace)?;
        (&arguments[..split], arguments[split..].trim())
    };
    let second = unquote_single_argument(remainder).trim();
    (!first.is_empty() && !second.is_empty()).then(|| (first.to_string(), second.to_string()))
}

fn format_elapsed(duration: Duration) -> String {
    if duration.as_secs_f64() < 1.0 {
        format!("{:.2} ms", duration.as_secs_f64() * 1_000.0)
    } else {
        format!("{:.2} s", duration.as_secs_f64())
    }
}

fn log_jit_report(command: &str, report: crate::basic_compat::JitExecutionReport) {
    let summary = if report.strict_native {
        format!(
            "{command} strict native: {} units, {} native calls, {} runtime helpers, {} interpreter statements, {} interpreter expressions (compile {}, run {}).",
            report.compiled_units.len(),
            report.compiled_calls,
            report.runtime_helper_calls,
            report.interpreted_statement_count,
            report.interpreted_expression_count,
            format_elapsed(report.compile_time),
            format_elapsed(report.compiled_time),
        )
    } else {
        let fallback = report
            .fallback_reason
            .unwrap_or_else(|| "remaining statements used the interpreter".into());
        format!(
            "{command}: {fallback}; {} interpreted statements, {} expressions (compile {}).",
            report.interpreted_statement_count,
            report.interpreted_expression_count,
            format_elapsed(report.compile_time),
        )
    };
    eprintln!("{summary}");
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;

    use super::*;

    fn put_word(block: &mut [u8], offset: usize, value: u32) {
        block[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    }

    fn get_word(task: &Task, address: u32) -> u32 {
        u32::from_le_bytes(
            task.memory
                .read_bytes(address, 4)
                .unwrap()
                .try_into()
                .unwrap(),
        )
    }

    fn initialise_wimp_task(dispatcher: &mut SwiDispatcher, task: &mut Task) {
        const DESCRIPTION: u32 = 0x1000;
        task.memory
            .write_bytes(DESCRIPTION, b"Graphics routing test\0")
            .unwrap();
        let mut context = SwiContext::default();
        context.registers[R0] = 310;
        context.registers[R1] = u32::from_le_bytes(*b"TASK");
        context.registers[R2] = DESCRIPTION;
        dispatcher
            .dispatch(WIMP_INITIALISE, task, &mut context)
            .unwrap();
    }

    fn create_wimp_window(
        dispatcher: &mut SwiDispatcher,
        task: &mut Task,
        block_address: u32,
        title: &str,
        area: WorkArea,
    ) -> u32 {
        let mut definition = [0_u8; 88];
        put_word(&mut definition, 0, area.min_x as u32);
        put_word(&mut definition, 4, area.min_y as u32);
        put_word(&mut definition, 8, area.max_x as u32);
        put_word(&mut definition, 12, area.max_y as u32);
        put_word(&mut definition, 28, 0xB600_0002);
        put_word(&mut definition, 40, 0);
        put_word(&mut definition, 44, (-512_i32) as u32);
        put_word(&mut definition, 48, 640);
        put_word(&mut definition, 52, 0);
        put_word(&mut definition, 56, 1);
        put_word(&mut definition, 60, 0);
        definition[72..72 + title.len()].copy_from_slice(title.as_bytes());
        task.memory.write_bytes(block_address, &definition).unwrap();
        let mut context = SwiContext::default();
        context.registers[R1] = block_address;
        dispatcher
            .dispatch(WIMP_CREATE_WINDOW, task, &mut context)
            .unwrap();
        let handle = context.registers[R0];

        let mut open = [0_u8; 32];
        put_word(&mut open, 0, handle);
        put_word(&mut open, 4, area.min_x as u32);
        put_word(&mut open, 8, area.min_y as u32);
        put_word(&mut open, 12, area.max_x as u32);
        put_word(&mut open, 16, area.max_y as u32);
        put_word(&mut open, 28, u32::MAX);
        task.memory
            .write_bytes(block_address + 0x100, &open)
            .unwrap();
        let mut context = SwiContext::default();
        context.registers[R1] = block_address + 0x100;
        dispatcher
            .dispatch(WIMP_OPEN_WINDOW, task, &mut context)
            .unwrap();
        handle
    }

    fn paint_next_wimp_redraw(
        dispatcher: &mut SwiDispatcher,
        task: &mut Task,
        address: u32,
        character: u8,
    ) -> (u32, i32, i32, u32) {
        let mut poll = SwiContext::default();
        poll.registers[R1] = address;
        dispatcher.dispatch(WIMP_POLL, task, &mut poll).unwrap();
        assert_eq!(poll.registers[R0], 1);
        let mut redraw = SwiContext::default();
        redraw.registers[R1] = address;
        dispatcher
            .dispatch(WIMP_REDRAW_WINDOW, task, &mut redraw)
            .unwrap();
        let handle = get_word(task, address);
        let colour = u32::from(character - b'A' + 1);
        let mut painted_point = None;
        while redraw.registers[R0] != 0 {
            for byte in [18, 0, colour as u8] {
                let mut output = SwiContext::default();
                output.registers[R0] = u32::from(byte);
                dispatcher.dispatch(OS_WRITE_C, task, &mut output).unwrap();
            }
            let clip = dispatcher
                .graphics()
                .snapshot()
                .wimp_clip
                .expect("Wimp redraw installs a graphics clip");
            let point = (clip.left + 1, clip.bottom + 1);
            let mut plot = SwiContext::default();
            plot.registers[R0] = 0x45;
            plot.registers[R1] = point.0 as u32;
            plot.registers[R2] = point.1 as u32;
            dispatcher.dispatch(OS_PLOT, task, &mut plot).unwrap();
            let mut read = SwiContext::default();
            read.registers[R0] = point.0 as u32;
            read.registers[R1] = point.1 as u32;
            dispatcher.dispatch(OS_READ_POINT, task, &mut read).unwrap();
            assert_eq!(
                (read.registers[R2], read.registers[R3], read.registers[R4]),
                (colour, 0, 0)
            );
            painted_point = Some(point);

            let mut output = SwiContext::default();
            output.registers[R0] = u32::from(character);
            dispatcher.dispatch(OS_WRITE_C, task, &mut output).unwrap();
            let mut rectangle = SwiContext::default();
            rectangle.registers[R1] = address;
            dispatcher
                .dispatch(WIMP_GET_RECTANGLE, task, &mut rectangle)
                .unwrap();
            redraw.registers[R0] = rectangle.registers[R0];
        }
        let (x, y) = painted_point.expect("the Wimp redraw had a visible rectangle");
        (handle, x, y, colour)
    }

    fn dispatch_cli_line(
        dispatcher: &mut SwiDispatcher,
        task: &mut Task,
        line: &str,
    ) -> Result<(), RuntimeError> {
        task.memory
            .write_bytes(CLI_STRING_BUFFER, line.as_bytes())?;
        task.memory
            .write_byte(CLI_STRING_BUFFER + line.len() as u32, 0)?;
        let mut context = SwiContext::default();
        context.registers[R0] = CLI_STRING_BUFFER;
        dispatcher.dispatch(OS_CLI, task, &mut context)
    }

    #[test]
    fn configure_accepts_supported_values_and_conf_abbreviation() {
        let (_input_sender, input_receiver) = mpsc::channel();
        let (display_sender, display_receiver) = mpsc::channel();
        let mut dispatcher =
            SwiDispatcher::windowed(HostConsole::windowed(input_receiver), display_sender);
        let config_path = std::env::temp_dir().join(format!(
            "acorn-2026-configure-cli-{}.txt",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&config_path);
        dispatcher.configure = ConfigureStore::with_path(&config_path);
        let mut task = Task::new(1);

        dispatch_cli_line(&mut dispatcher, &mut task, "*CONFIGURE BASICEngine Strict").unwrap();
        assert_eq!(
            dispatcher.configure.load().unwrap().engine,
            BasicEngine::StrictJit
        );

        dispatch_cli_line(&mut dispatcher, &mut task, "*CONF. BASICEngine Hybrid").unwrap();
        assert_eq!(
            dispatcher.configure.load().unwrap().engine,
            BasicEngine::HybridJit
        );

        dispatch_cli_line(&mut dispatcher, &mut task, "*CONFIGURE Language 3").unwrap();
        assert_eq!(
            dispatcher
                .configure
                .load()
                .unwrap()
                .status_value("Language")
                .unwrap()
                .1,
            "3"
        );
        let _ = display_receiver.try_iter().count();
        dispatch_cli_line(&mut dispatcher, &mut task, "*STATUS Language").unwrap();
        let status_output = display_receiver
            .try_iter()
            .filter_map(|event| match event {
                DisplayEvent::WriteByte { byte, .. } => Some(byte),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert!(String::from_utf8_lossy(&status_output).contains("Language=3"));

        for value in ["HybridJIT", "Hybrid-JIT", "StrictJIT", "Strict-JIT"] {
            let command = format!("*CONFIGURE BASICEngine {value}");
            dispatch_cli_line(&mut dispatcher, &mut task, &command).unwrap();
            assert_eq!(
                dispatcher.configure.load().unwrap().engine,
                BasicEngine::HybridJit,
                "accepted {value}"
            );
        }
        let _ = std::fs::remove_file(config_path);
    }

    #[test]
    fn os_read_point_reads_immediate_pixels_and_preserves_coordinates() {
        let mut dispatcher = SwiDispatcher::new(HostConsole::stdio());
        let mut task = Task::new(1);
        for byte in [22_u8, 2, 18, 0, 2] {
            let mut output = SwiContext::default();
            output.registers[R0] = u32::from(byte);
            dispatcher
                .dispatch(OS_WRITE_C, &mut task, &mut output)
                .unwrap();
        }
        assert_eq!(dispatcher.graphics().snapshot().graphics_colour, 2);
        for byte in [29_u8, 5, 0, 7, 0] {
            let mut output = SwiContext::default();
            output.registers[R0] = u32::from(byte);
            dispatcher
                .dispatch(OS_WRITE_C, &mut task, &mut output)
                .unwrap();
        }
        let mut plot = SwiContext::default();
        plot.registers[R0] = 0x45;
        plot.registers[R1] = 10;
        plot.registers[R2] = 20;
        dispatcher.dispatch(OS_PLOT, &mut task, &mut plot).unwrap();
        assert!(dispatcher.graphics().snapshot().graphics_content_present);
        let mut read = SwiContext::default();
        read.registers[R0] = 10;
        read.registers[R1] = 20;
        dispatcher
            .dispatch_named_swi("OS_READPOINT", &mut task, &mut read)
            .unwrap();
        assert_eq!((read.registers[R0], read.registers[R1]), (10, 20));
        assert_eq!(
            (read.registers[R2], read.registers[R3], read.registers[R4]),
            (2, 0, 0)
        );

        let mut off_screen = SwiContext::default();
        off_screen.registers[R0] = (-10_000_i32) as u32;
        off_screen.registers[R1] = 20;
        dispatcher
            .dispatch(OS_READ_POINT, &mut task, &mut off_screen)
            .unwrap();
        assert_eq!(off_screen.registers[R0], (-10_000_i32) as u32);
        assert_eq!(off_screen.registers[R1], 20);
        assert_eq!(off_screen.registers[R2], u32::MAX);
        assert_eq!(off_screen.registers[R4], u32::MAX);
    }

    #[test]
    fn wimp_redraw_routes_to_independent_window_surfaces_and_restores_task_default() {
        let (input_sender, input_receiver) = mpsc::channel();
        let (display_sender, _display_receiver) = mpsc::channel();
        let (desktop_sender, _desktop_receiver) = mpsc::channel();
        let wimp = WimpServer::new(desktop_sender);
        let mut dispatcher = SwiDispatcher::desktop_task(
            HostConsole::windowed(input_receiver),
            display_sender,
            77,
            wimp,
        );
        let mut task = Task::new(77);
        for byte in [22_u8, 2] {
            let mut output = SwiContext::default();
            output.registers[R0] = u32::from(byte);
            dispatcher
                .dispatch(OS_WRITE_C, &mut task, &mut output)
                .unwrap();
        }
        initialise_wimp_task(&mut dispatcher, &mut task);

        let first = create_wimp_window(
            &mut dispatcher,
            &mut task,
            0x2000,
            "First",
            WorkArea {
                min_x: 100,
                min_y: 400,
                max_x: 600,
                max_y: 900,
            },
        );
        let second = create_wimp_window(
            &mut dispatcher,
            &mut task,
            0x2200,
            "Second",
            WorkArea {
                min_x: 700,
                min_y: 400,
                max_x: 1200,
                max_y: 900,
            },
        );

        let (first_painted, first_x, first_y, first_colour) =
            paint_next_wimp_redraw(&mut dispatcher, &mut task, 0x2400, b'A');
        let (second_painted, second_x, second_y, second_colour) =
            paint_next_wimp_redraw(&mut dispatcher, &mut task, 0x2600, b'B');
        assert_eq!([first_painted, second_painted], [first, second]);
        assert_eq!(
            dispatcher.window_graphics[&first].read_point(first_x, first_y),
            Some((first_colour, 0))
        );
        assert_eq!(
            dispatcher.window_graphics[&second].read_point(second_x, second_y),
            Some((second_colour, 0))
        );
        assert_eq!(dispatcher.active_graphics_window, None);
        assert_eq!(dispatcher.graphics().snapshot().text_cells[0], b' ');
        assert_eq!(
            dispatcher.window_graphics[&first].snapshot().text_cells[0],
            b'A'
        );
        assert_eq!(
            dispatcher.window_graphics[&second].snapshot().text_cells[0],
            b'B'
        );

        let mut update_block = [0_u8; 44];
        put_word(&mut update_block, 0, first);
        put_word(&mut update_block, 4, 0);
        put_word(&mut update_block, 8, (-512_i32) as u32);
        put_word(&mut update_block, 12, 640);
        put_word(&mut update_block, 16, 0);
        task.memory.write_bytes(0x2800, &update_block).unwrap();
        let mut update = SwiContext::default();
        update.registers[R1] = 0x2800;
        dispatcher
            .dispatch(WIMP_UPDATE_WINDOW, &mut task, &mut update)
            .unwrap();
        while update.registers[R0] != 0 {
            for byte in [31, 1, 0, b'U'] {
                let mut output = SwiContext::default();
                output.registers[R0] = u32::from(byte);
                dispatcher
                    .dispatch(OS_WRITE_C, &mut task, &mut output)
                    .unwrap();
            }
            let mut rectangle = SwiContext::default();
            rectangle.registers[R1] = 0x2800;
            dispatcher
                .dispatch(WIMP_GET_RECTANGLE, &mut task, &mut rectangle)
                .unwrap();
            update.registers[R0] = rectangle.registers[R0];
        }
        assert_eq!(dispatcher.active_graphics_window, None);
        assert_eq!(
            dispatcher.window_graphics[&first].snapshot().text_cells[0],
            b'A'
        );
        assert_eq!(
            dispatcher.window_graphics[&first].snapshot().text_cells[1],
            b'U'
        );

        let mut default_output = SwiContext::default();
        default_output.registers[R0] = u32::from(b'D');
        dispatcher
            .dispatch(OS_WRITE_C, &mut task, &mut default_output)
            .unwrap();
        assert_eq!(dispatcher.graphics().snapshot().text_cells[0], b'D');
        assert_eq!(
            dispatcher.window_graphics[&first].snapshot().text_cells[0],
            b'A'
        );
        assert_eq!(
            dispatcher.window_graphics[&second].snapshot().text_cells[0],
            b'B'
        );
        drop(input_sender);
    }

    #[test]
    fn wimp_redraw_clip_maps_half_open_work_area_to_local_graphics_coordinates() {
        let clip = work_area_to_graphics_clip(
            WorkArea {
                min_x: 10,
                min_y: -300,
                max_x: 210,
                max_y: -100,
            },
            WorkArea {
                min_x: 10,
                min_y: -600,
                max_x: 400,
                max_y: 0,
            },
            512,
        );
        assert_eq!(
            clip,
            GraphicsWindow {
                left: 0,
                bottom: 212,
                right: 199,
                top: 411,
            }
        );
    }

    #[test]
    fn desktop_cli_requires_the_windowed_wimp_service_and_accepts_only_no_arguments() {
        let (_input_sender, input_receiver) = mpsc::channel();
        let (display_sender, _display_receiver) = mpsc::channel();
        let mut dispatcher =
            SwiDispatcher::windowed(HostConsole::windowed(input_receiver), display_sender);
        let mut task = Task::new(1);

        let error = dispatch_cli_line(&mut dispatcher, &mut task, "*dEsK.").unwrap_err();
        assert!(
            error
                .to_string()
                .contains("DESKTOP requires the windowed host")
        );
        assert!(!dispatcher.desktop_requested());

        assert!(dispatch_cli_line(&mut dispatcher, &mut task, "DESKTOP extra").is_ok());
        assert!(!dispatcher.desktop_requested());
    }

    #[test]
    fn desktop_abbreviation_does_not_steal_the_existing_dir_abbreviation() {
        let (_input_sender, input_receiver) = mpsc::channel();
        let (display_sender, _display_receiver) = mpsc::channel();
        let mut dispatcher =
            SwiDispatcher::windowed(HostConsole::windowed(input_receiver), display_sender);
        let mut task = Task::new(1);

        dispatch_cli_line(&mut dispatcher, &mut task, "*D.").unwrap();
        assert!(!dispatcher.desktop_requested());
    }

    #[test]
    fn basic64_source_type_is_named_and_launchable_without_tokenisation() {
        assert_eq!(parse_file_type("BASIC64").unwrap(), FILETYPE_BASIC64);
        assert_eq!(parse_file_type("&064").unwrap(), FILETYPE_BASIC64);
        assert_eq!(file_type_name(FILETYPE_BASIC64), *b"BASIC64 ");
        assert_ne!(FILETYPE_BASIC64, FILETYPE_BASIC);
        let mut dispatcher = SwiDispatcher::new(HostConsole::stdio());
        dispatcher.set_file_system_for_test(HostFileSystem::new(
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("demo-volume"),
        ));
        let mut task = Task::new(31);
        task.memory.write_bytes(0x1800, b"$.System\0").unwrap();
        for index in 0..2 {
            let mut context = SwiContext::default();
            context.registers[R0] = 1;
            context.registers[R1] = 0x1800;
            context.registers[R2] = index;
            context.registers[R3] = 0x1900;
            context.registers[R4] = 256;
            dispatcher
                .dispatch_named_swi("ACORN_DESKTOP", &mut task, &mut context)
                .unwrap();
            assert_eq!(
                context.registers[R0], 1,
                "BASIC64 source must be launchable"
            );
            assert_eq!(context.registers[R1], FILETYPE_BASIC64);
        }
    }

    #[test]
    fn acorn_desktop_catalogue_uses_checked_guest_buffers_and_hostfs_metadata() {
        let mut dispatcher = SwiDispatcher::new(HostConsole::stdio());
        dispatcher.set_file_system_for_test(HostFileSystem::new(
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("demo-volume"),
        ));
        let mut task = Task::new(31);
        const DIRECTORY: u32 = 0x1800;
        const ENTRY: u32 = 0x1900;
        task.memory.write_bytes(DIRECTORY, b"$.Examples\0").unwrap();

        let mut context = SwiContext::default();
        context.registers[R0] = 1;
        context.registers[R1] = DIRECTORY;
        context.registers[R2] = 0;
        context.registers[R3] = ENTRY;
        context.registers[R4] = 256;
        dispatcher
            .dispatch_named_swi("ACORN_DESKTOP", &mut task, &mut context)
            .unwrap();
        assert!([1, 2, 3, 4].contains(&context.registers[R0]));
        let name = task.memory.read_c_string(ENTRY, 256).unwrap();
        assert!(!name.is_empty());
        if context.registers[R0] != 2 {
            assert!(matches!(
                context.registers[R1] & 0xFFF,
                FILETYPE_BASIC | FILETYPE_BASIC64 | FILETYPE_TEXT
            ));
        }

        let mut date = SwiContext::default();
        date.registers[R0] = 3;
        date.registers[R1] = DIRECTORY;
        date.registers[R2] = 0;
        dispatcher
            .dispatch_named_swi("ACORN_DESKTOP", &mut task, &mut date)
            .unwrap();
        assert_eq!(date.registers[R0], 1);
        assert!(date.registers[R1] > 0);
        date.registers[R0] = 3;
        date.registers[R1] = DIRECTORY;
        date.registers[R2] = u32::MAX;
        dispatcher
            .dispatch_named_swi("ACORN_DESKTOP", &mut task, &mut date)
            .unwrap();
        assert_eq!((date.registers[R0], date.registers[R1]), (0, 0));

        task.memory.write_bytes(DIRECTORY, b"../../etc\0").unwrap();
        let mut invalid = SwiContext::default();
        invalid.registers[R0] = 1;
        invalid.registers[R1] = DIRECTORY;
        invalid.registers[R3] = ENTRY;
        invalid.registers[R4] = 256;
        assert!(
            dispatcher
                .dispatch_named_swi("ACORN_DESKTOP", &mut task, &mut invalid)
                .is_err()
        );

        task.memory.write_bytes(DIRECTORY, b"$.Examples\0").unwrap();
        let mut bad_buffer = SwiContext::default();
        bad_buffer.registers[R0] = 1;
        bad_buffer.registers[R1] = DIRECTORY;
        bad_buffer.registers[R3] = u32::MAX - 4;
        bad_buffer.registers[R4] = 256;
        assert!(
            dispatcher
                .dispatch_named_swi("ACORN_DESKTOP", &mut task, &mut bad_buffer)
                .is_err()
        );
    }

    #[test]
    fn clocksp5_compact_fx_reset_is_a_quiet_hosted_noop() {
        let (_input_sender, input_receiver) = mpsc::channel();
        let (display_sender, display_receiver) = mpsc::channel();
        let mut dispatcher =
            SwiDispatcher::windowed(HostConsole::windowed(input_receiver), display_sender);
        let mut task = Task::new(1);

        dispatch_cli_line(&mut dispatcher, &mut task, "*fx151,78,243").unwrap();
        assert!(display_receiver.try_iter().next().is_none());
    }

    #[cfg(feature = "experimental-jit")]
    #[test]
    fn strict_basicjit_returns_to_cli_with_loaded_program_available() {
        let (_input_sender, input_receiver) = mpsc::channel();
        let (display_sender, display_receiver) = mpsc::channel();
        let mut dispatcher =
            SwiDispatcher::windowed(HostConsole::windowed(input_receiver), display_sender);
        let mut task = Task::new(1);
        task.loaded_tokenized_program = Some(crate::tokenized_basic::TokenizedBasicProgram {
            lines: vec![
                crate::tokenized_basic::TokenizedBasicLine {
                    number: 10,
                    bytes: vec![0xF1, b'"', b'S', b'T', b'R', b'I', b'C', b'T', b'"'],
                    line_references: Vec::new(),
                },
                crate::tokenized_basic::TokenizedBasicLine {
                    number: 20,
                    bytes: vec![0xE0],
                    line_references: Vec::new(),
                },
            ],
            record_layout: None,
        });

        dispatch_cli_line(
            &mut dispatcher,
            &mut task,
            "BASICJIT STRICT --benchmark-validation",
        )
        .unwrap();
        assert!(task.loaded_tokenized_program.is_some());
        let native_output = display_receiver
            .try_iter()
            .filter_map(|event| match event {
                DisplayEvent::WriteByte { byte, .. } => Some(byte),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert!(native_output.windows(6).any(|bytes| bytes == b"STRICT"));
        dispatch_cli_line(&mut dispatcher, &mut task, "HELP").unwrap();
        dispatch_cli_line(&mut dispatcher, &mut task, "QUIT").unwrap();
        assert!(dispatcher.quit_requested());
    }
}
