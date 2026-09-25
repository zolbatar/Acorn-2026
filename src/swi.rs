use crate::{
    error::RuntimeError,
    graphics::GraphicsService,
    host::HostConsole,
    memory::{GUEST_MEMORY_BASE, Task},
};
use std::sync::mpsc::Sender;

pub const OS_WRITE_C: u32 = 0x00;
pub const OS_WRITE_S: u32 = 0x01;
pub const OS_WRITE_0: u32 = 0x02;
pub const OS_NEW_LINE: u32 = 0x03;
pub const OS_READ_C: u32 = 0x04;
pub const OS_CLI: u32 = 0x05;
pub const OS_READ_LINE: u32 = 0x0E;
pub const OS_PLOT: u32 = 0x45;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DisplayEvent {
    WriteByte(u8),
    Plot { code: u8, x: i32, y: i32 },
    RuntimeExited,
}

const R0: usize = 0;
const R1: usize = 1;
const R2: usize = 2;
const R3: usize = 3;
const R4: usize = 4;
const GUEST_ADDRESS_MASK: u32 = 0x3FFF_FFFF;
const READ_LINE_ECHO_ONLY_BUFFERED: u32 = 1 << 31;
const READ_LINE_ECHO_R4: u32 = 1 << 30;
const MAX_CLI_BYTES: usize = 256;
const MAX_STRING_BYTES: usize = 4096;
const OUTPUT_BUFFER: u32 = GUEST_MEMORY_BASE + 0x1000;
const HELP_TEXT: &[u8] =
    b"Acorn-2026 MOS commands:\n\r  HELP       Show this help.\n\r  RUN        Run a .bas64 source file.\n\r  BASICLOAD  Load a tokenised BASIC file.\n\r  BASICRUN   Run the loaded compatibility subset.\n\r  QUIT       Exit the runtime.";

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
    quit_requested: bool,
    display_events: Option<Sender<DisplayEvent>>,
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
            quit_requested: false,
            display_events,
        }
    }

    pub fn quit_requested(&self) -> bool {
        self.quit_requested
    }

    pub fn graphics(&self) -> &GraphicsService {
        &self.graphics
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
                let output_byte = self.graphics.write_byte(character)?;
                self.publish_display_event(DisplayEvent::WriteByte(character));
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
            OS_PLOT => {
                let code = context.registers[R0] as u8;
                let x = context.registers[R1] as i32;
                let y = context.registers[R2] as i32;
                self.graphics.plot(code, x, y)?;
                self.publish_display_event(DisplayEvent::Plot { code, x, y });
                Ok(())
            }
            other => Err(RuntimeError::InvalidSwi(other)),
        }
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
        if command.is_empty() {
            return Ok(());
        }

        let mut words = command.splitn(2, char::is_whitespace);
        let verb = words.next().unwrap_or_default();
        let arguments = words.next().unwrap_or_default().trim();

        if verb.eq_ignore_ascii_case("HELP") && arguments.is_empty() {
            self.write_inline(task, HELP_TEXT)?;
            self.write_new_line(task)
        } else if verb.eq_ignore_ascii_case("QUIT") && arguments.is_empty() {
            self.quit_requested = true;
            Ok(())
        } else if verb.eq_ignore_ascii_case("RUN") {
            if arguments.is_empty() {
                self.write_inline(task, b"Syntax: RUN <file.bas64>")?;
                return self.write_new_line(task);
            }

            let path = arguments
                .strip_prefix('"')
                .and_then(|path| path.strip_suffix('"'))
                .unwrap_or(arguments);
            match crate::basic64::run_file(path, task, self) {
                Ok(()) => Ok(()),
                Err(error) => {
                    let message = format!("BASIC64 error: {error}");
                    self.write_inline(task, message.as_bytes())?;
                    self.write_new_line(task)
                }
            }
        } else if verb.eq_ignore_ascii_case("BASICLOAD") {
            if arguments.is_empty() {
                self.write_inline(task, b"Syntax: BASICLOAD <file>")?;
                return self.write_new_line(task);
            }

            let path = arguments
                .strip_prefix('"')
                .and_then(|path| path.strip_suffix('"'))
                .unwrap_or(arguments);
            match crate::tokenized_basic::TokenizedBasicProgram::load_file(path) {
                Ok(program) => {
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
                Err(error) => {
                    let message = format!("BASICLOAD error: {error}");
                    self.write_inline(task, message.as_bytes())?;
                    self.write_new_line(task)
                }
            }
        } else if verb.eq_ignore_ascii_case("BASICRUN") {
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
            let result = crate::basic_compat::run_program(&program, task, self);
            task.loaded_tokenized_program = Some(program);
            match result {
                Ok(()) => Ok(()),
                Err(error) => {
                    let message = format!("BASICRUN error: {error}");
                    self.write_inline(task, message.as_bytes())?;
                    self.write_new_line(task)
                }
            }
        } else {
            self.write_inline(task, b"Bad command")?;
            self.write_new_line(task)
        }
    }

    fn write_new_line(&mut self, task: &mut Task) -> Result<(), RuntimeError> {
        self.dispatch(OS_NEW_LINE, task, &mut SwiContext::default())
    }
}
