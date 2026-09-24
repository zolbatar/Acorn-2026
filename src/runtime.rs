use crate::{
    error::RuntimeError,
    host::HostConsole,
    memory::{GUEST_MEMORY_BASE, Task},
    swi::{OS_CLI, OS_READ_LINE, OS_WRITE_C, SwiContext, SwiDispatcher},
};

const TASK_ID: u64 = 1;
const LINE_BUFFER: u32 = GUEST_MEMORY_BASE;
const LINE_BUFFER_SIZE: u32 = 256;

pub struct Runtime {
    task: Task,
    dispatcher: SwiDispatcher,
}

impl Runtime {
    pub fn stdio() -> Self {
        Self::new(HostConsole::stdio())
    }

    pub fn new(console: HostConsole) -> Self {
        Self {
            task: Task::new(TASK_ID),
            dispatcher: SwiDispatcher::new(console),
        }
    }

    pub fn run(&mut self) -> Result<(), RuntimeError> {
        loop {
            let mut prompt = SwiContext::default();
            prompt.registers[0] = u32::from(b'*');
            self.dispatcher
                .dispatch(OS_WRITE_C, &mut self.task, &mut prompt)?;
            self.dispatcher.flush()?;

            let mut input = SwiContext::default();
            input.registers[0] = LINE_BUFFER;
            input.registers[1] = LINE_BUFFER_SIZE - 1;
            input.registers[2] = u32::from(b' ');
            input.registers[3] = u32::from(b'~');

            match self
                .dispatcher
                .dispatch(OS_READ_LINE, &mut self.task, &mut input)
            {
                Err(RuntimeError::EndOfInput) => return Ok(()),
                Err(error) => return Err(error),
                Ok(()) => {}
            }

            if input.carry {
                continue;
            }

            let length = input.registers[1];
            if length == 0 {
                continue;
            }
            let terminator = LINE_BUFFER
                .checked_add(length)
                .ok_or(crate::memory::MemoryError::AddressOverflow)?;
            self.task.memory.write_byte(terminator, 0)?;

            let mut command = SwiContext::default();
            command.registers[0] = LINE_BUFFER;
            self.dispatcher
                .dispatch(OS_CLI, &mut self.task, &mut command)?;
            if self.dispatcher.quit_requested() {
                return Ok(());
            }
        }
    }

    pub fn report_error(&mut self, error: &RuntimeError) -> Result<(), RuntimeError> {
        let message = format!("Acorn-2026 error: {error}");
        self.dispatcher
            .write_inline(&mut self.task, message.as_bytes())?;
        self.dispatcher.dispatch(
            crate::swi::OS_NEW_LINE,
            &mut self.task,
            &mut SwiContext::default(),
        )?;
        self.dispatcher.flush()
    }
}
