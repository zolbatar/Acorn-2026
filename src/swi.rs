use crate::{
    error::RuntimeError,
    filesystem::{FILETYPE_BASIC, FILETYPE_TEXT, FileMetadata, HostFileSystem, OpenFile},
    graphics::{GraphicsProfile, GraphicsService, GraphicsSnapshot},
    host::HostConsole,
    memory::{GUEST_MEMORY_BASE, Task},
};
use std::{
    io::{Read, Seek, SeekFrom, Write},
    sync::mpsc::Sender,
    time::{Duration, Instant},
};

const DISPLAY_BATCH_FRAME_INTERVAL: Duration = Duration::from_micros(16_667);
const INKEY_POLL_INTERVAL: Duration = Duration::from_millis(8);
const MAX_EXTENDED_MODE_PIXELS: u64 = 4_194_304;

pub const OS_WRITE_C: u32 = 0x00;
pub const OS_WRITE_S: u32 = 0x01;
pub const OS_WRITE_0: u32 = 0x02;
pub const OS_NEW_LINE: u32 = 0x03;
pub const OS_READ_C: u32 = 0x04;
pub const OS_CLI: u32 = 0x05;
pub const OS_FILE: u32 = 0x08;
pub const OS_ARGS: u32 = 0x09;
pub const OS_BGET: u32 = 0x0A;
pub const OS_BPUT: u32 = 0x0B;
pub const OS_GBPB: u32 = 0x0C;
pub const OS_FIND: u32 = 0x0D;
pub const OS_READ_LINE: u32 = 0x0E;
pub const OS_FSCONTROL: u32 = 0x29;
pub const OS_PLOT: u32 = 0x45;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DisplayEvent {
    WriteByte(u8),
    Plot { code: u8, x: i32, y: i32 },
    GraphicsSnapshot(GraphicsSnapshot),
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
const HELP_TEXT: &[u8] = b"Acorn-2026 MOS commands:\n\r  Commands can be abbreviated with a final dot (for example, *CA.); *. is a shortcut for *CAT.\n\r  *CAT [dir]             Catalogue a directory.\n\r  *DIR [dir]             Select the current directory.\n\r  *CDIR <dir>            Create a directory.\n\r  *DELETE <file>         Delete a file.\n\r  *RENAME <old> <new>    Rename a file or directory.\n\r  *FILETYPE <file> <id>  Set a RISC OS file type.\n\r  *TYPE <file>           Display a text file.\n\r  *DISC [name]           Read or set the volume name.\n\r  *HOSTFS                Select the HostFS filing system.\n\r  RUN <file>             Run a BASIC source or tokenised file.\n\r  BASICLOAD <file>       Load a tokenised BASIC program.\n\r  BASICRUN               Run the loaded program.\n\r  BASICJIT [file]        Run with experimental native hot regions.\n\r  HELP                   Show this help.\n\r  QUIT                   Exit the runtime.";

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
    console: HostConsole,
    graphics: GraphicsService,
    file_system: HostFileSystem,
    quit_requested: bool,
    display_events: Option<Sender<DisplayEvent>>,
    display_batch_active: bool,
    last_display_batch_publish: Option<Instant>,
    last_inkey_poll: Instant,
}

impl SwiDispatcher {
    pub fn new(console: HostConsole) -> Self {
        Self::with_display_events(console, None)
    }

    pub fn windowed(console: HostConsole, display_events: Sender<DisplayEvent>) -> Self {
        Self::with_display_events(console, Some(display_events))
    }

    fn with_display_events(
        console: HostConsole,
        display_events: Option<Sender<DisplayEvent>>,
    ) -> Self {
        Self {
            console,
            graphics: GraphicsService::default(),
            file_system: HostFileSystem::demo_default(),
            quit_requested: false,
            display_events,
            display_batch_active: false,
            last_display_batch_publish: None,
            last_inkey_poll: Instant::now(),
        }
    }

    pub fn quit_requested(&self) -> bool {
        self.quit_requested
    }

    pub fn graphics(&self) -> &GraphicsService {
        &self.graphics
    }

    pub(crate) fn set_graphics_profile(
        &mut self,
        profile: GraphicsProfile,
    ) -> Result<(), RuntimeError> {
        let previous = self.graphics.snapshot().clone();
        self.graphics.set_profile(profile)?;
        let snapshot = self.graphics.snapshot().clone();
        if snapshot != previous {
            self.publish_display_event(DisplayEvent::GraphicsSnapshot(snapshot));
        }
        Ok(())
    }

    pub(crate) fn poll_key(&mut self) -> Option<u8> {
        if self.last_inkey_poll.elapsed() < INKEY_POLL_INTERVAL {
            return None;
        }
        self.last_inkey_poll = Instant::now();
        self.console.try_read_byte()
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
        self.graphics.set_extended_mode(
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
                self.graphics.set_rgb_gcol(context.registers[R0]);
                Ok(())
            }
            "COLOURTRANS_WRITEPALETTE" => Ok(()),
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
        match number {
            OS_WRITE_C => {
                let character = context.registers[R0] as u8;
                let previous_mode = self.graphics.snapshot().mode;
                let output_byte = self.graphics.write_byte(character)?;
                let mode_changed = self.graphics.snapshot().mode != previous_mode;
                if mode_changed {
                    self.publish_display_event(DisplayEvent::GraphicsSnapshot(
                        self.graphics.snapshot().clone(),
                    ));
                } else if !self.display_batch_active || output_byte.is_some() {
                    self.publish_display_event(DisplayEvent::WriteByte(character));
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
                self.graphics.plot(code, x, y)?;
                if self.display_batch_active {
                    self.publish_display_batch_snapshot_if_due();
                } else {
                    self.publish_display_event(DisplayEvent::Plot { code, x, y });
                }
                Ok(())
            }
            other => Err(RuntimeError::InvalidSwi(other)),
        }
    }

    pub(crate) fn read_guest_file(
        &self,
        task: &Task,
        path: &str,
    ) -> Result<(Vec<u8>, FileMetadata), RuntimeError> {
        self.file_system.read_file(&task.file_system, path)
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

        let path = read_guest_string(task, context.registers[R1])?;
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
            self.publish_display_event(DisplayEvent::GraphicsSnapshot(
                self.graphics.snapshot().clone(),
            ));
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

        let snapshot = self.graphics.snapshot().clone();
        self.publish_display_event(DisplayEvent::GraphicsSnapshot(snapshot));
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
        let byte = self.console.read_byte()?.ok_or(RuntimeError::EndOfInput)?;
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
        let bytes = task
            .memory
            .read_c_string(context.registers[R0], MAX_CLI_BYTES)?;
        let command = String::from_utf8_lossy(&bytes);
        let command = command.trim();
        let command = command.trim_start_matches('*').trim_start();
        if command.is_empty() {
            return Ok(());
        }

        let mut words = command.splitn(2, char::is_whitespace);
        let verb = words.next().unwrap_or_default();
        let arguments = words.next().unwrap_or_default().trim();

        if cli_command_matches(verb, "HELP") && arguments.is_empty() {
            self.write_inline(task, HELP_TEXT)?;
            self.write_new_line(task)
        } else if cli_command_matches(verb, "QUIT") && arguments.is_empty() {
            self.quit_requested = true;
            Ok(())
        } else if cli_command_matches(verb, "RUN") {
            if arguments.is_empty() {
                self.write_inline(task, b"Syntax: RUN <file.bas64|bas|txt|asc|bbc>")?;
                return self.write_new_line(task);
            }

            let path = unquote_single_argument(arguments);
            self.begin_display_batch();
            match crate::basic64::run_guest_file(path, task, self) {
                Ok(()) => {
                    self.finish_display_batch();
                    Ok(())
                }
                Err(error) => {
                    self.finish_display_batch();
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
            let result = crate::basic_compat::run_program(&program, task, self);
            self.finish_display_batch();
            task.loaded_tokenized_program = Some(program);
            match result {
                Ok(()) => Ok(()),
                Err(error) => {
                    let message = format!("BASICRUN error: {error}");
                    self.write_inline(task, message.as_bytes())?;
                    self.write_new_line(task)
                }
            }
        } else if cli_command_matches(verb, "BASICJIT") {
            eprintln!(
                "BASICJIT: compiling verified native regions; unmatched BASIC stays interpreted."
            );
            self.begin_display_batch();
            let result = if arguments.is_empty() {
                let Some(program) = task.loaded_tokenized_program.take() else {
                    self.finish_display_batch();
                    self.write_inline(
                        task,
                        b"No tokenised BASIC program is loaded; use BASICLOAD or pass a file.",
                    )?;
                    return self.write_new_line(task);
                };
                let result = crate::basic_compat::run_program_jit(&program, task, self);
                task.loaded_tokenized_program = Some(program);
                result
            } else {
                let path = unquote_single_argument(arguments);
                crate::basic64::run_guest_file_jit(path, task, self)
            };
            self.finish_display_batch();
            match result {
                Ok(report) => {
                    let units = if report.compiled_units.is_empty() {
                        "none".to_string()
                    } else {
                        report.compiled_units.join(", ")
                    };
                    let fallback = report
                        .fallback_reason
                        .unwrap_or_else(|| "remaining statements used the interpreter".into());
                    let native_call_count = if report.compiled_calls == 1 {
                        "1 native call".to_string()
                    } else {
                        format!("{} native calls", report.compiled_calls)
                    };
                    let native_work = if report.rendered_pixels == 0 {
                        native_call_count
                    } else {
                        format!("{native_call_count} for {} pixels", report.rendered_pixels)
                    };
                    let summary = format!(
                        "BASICJIT: compiled {units}; {native_work} in {} (compile {}); {fallback}.",
                        format_elapsed(report.compiled_time),
                        format_elapsed(report.compile_time),
                    );
                    eprintln!("{summary}");
                    Ok(())
                }
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

    fn write_new_line(&mut self, task: &mut Task) -> Result<(), RuntimeError> {
        self.dispatch(OS_NEW_LINE, task, &mut SwiContext::default())
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
