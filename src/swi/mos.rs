//! BBC MOS entrypoint adapters. Addresses here identify services, never host code.
use super::*;
use std::{
    collections::VecDeque,
    sync::atomic::{AtomicU64, Ordering},
};

const CLOCK_MASK: u64 = (1_u64 << 40) - 1;

#[derive(Clone)]
pub(crate) struct MosClock(Arc<ClockState>);

struct ClockState {
    started: Instant,
    offset: AtomicU64,
}

impl Default for MosClock {
    fn default() -> Self {
        Self(Arc::new(ClockState {
            started: Instant::now(),
            offset: AtomicU64::new(0),
        }))
    }
}

impl MosClock {
    pub(crate) fn read(&self) -> u64 {
        self.0
            .offset
            .load(Ordering::Relaxed)
            .wrapping_add(self.0.started.elapsed().as_millis() as u64 / 10)
            & CLOCK_MASK
    }

    pub(crate) fn set(&self, value: u64) {
        let elapsed = self.0.started.elapsed().as_millis() as u64 / 10;
        self.0
            .offset
            .store(value.wrapping_sub(elapsed) & CLOCK_MASK, Ordering::Relaxed);
    }
}

#[derive(Default)]
pub(super) struct MosState {
    pub(super) system_clock: MosClock,
    pub(super) monotonic_timer: MosClock,
    pub(super) interval_timer: MosClock,
    pub(super) input: VecDeque<u8>,
}

impl MosState {
    pub(super) fn insert_key(&mut self, byte: u8) -> bool {
        if self.input.len() >= 256 {
            true
        } else {
            self.input.push_back(byte);
            false
        }
    }

    pub(super) fn flush_keyboard(&mut self, console: &HostConsole) {
        self.input.clear();
        while console.try_read_byte().is_some() {}
    }

    /// Poll the MOS keyboard queue for the requested centisecond interval.
    /// An active Exec source owns this task's input stream, so timed key
    /// polling must neither advance it nor consume queued host input.
    pub(super) fn read_timed_key(
        &mut self,
        task: &Task,
        console: &HostConsole,
        timeout_centiseconds: u16,
    ) -> (u8, u8) {
        let deadline = Instant::now() + Duration::from_millis(u64::from(timeout_centiseconds) * 10);
        loop {
            if !task.has_exec_input()
                && let Some(key) = self.input.pop_front().or_else(|| console.try_read_byte())
            {
                return (key, if key == 27 { 1 } else { 0 });
            }
            if Instant::now() >= deadline {
                return (255, 2);
            }
            std::thread::sleep(Duration::from_millis(1));
        }
    }
}

// ARM BASIC accepts a complete address in X%, or a low byte in X% and
// the remaining address bits in Y%. Do not truncate Y% to a 6502 byte.
fn xy_address(x: u32, y: u32) -> Result<u32, RuntimeError> {
    if x >= 256 {
        Ok(x)
    } else {
        u32::try_from(u64::from(x) + (u64::from(y) << 8))
            .map_err(|_| crate::memory::MemoryError::AddressOverflow.into())
    }
}

pub(super) fn read_mos_string(
    task: &Task,
    address: u32,
    maximum: usize,
) -> Result<Vec<u8>, RuntimeError> {
    let mut bytes = Vec::new();
    for offset in 0..maximum {
        let current = address
            .checked_add(offset as u32)
            .ok_or(crate::memory::MemoryError::AddressOverflow)?;
        let byte = task.memory.read_byte(current)?;
        if byte == 0 || byte == 13 {
            return Ok(bytes);
        }
        bytes.push(byte);
    }
    Err(RuntimeError::Program(format!(
        "MOS string at &{address:X} has no terminator within {maximum} bytes"
    )))
}

impl SwiDispatcher {
    pub(crate) fn restore_polled_key(&mut self, key: u8) {
        self.mos.input.push_front(key);
    }

    pub(crate) fn system_clock(&self) -> MosClock {
        self.mos.system_clock.clone()
    }

    /// Translate the parameterless BASIC CALL convention to checked SWIs.
    /// A future native compiler can call this same service boundary directly.
    /// Register results remain in `context`; CALL itself does not copy them
    /// back into BASIC variables (unlike a SYS ... TO result list).
    pub fn dispatch_mos_call(
        &mut self,
        address: u32,
        task: &mut Task,
        context: &mut SwiContext,
    ) -> Result<(), RuntimeError> {
        let a = context.registers[R0];
        let x = context.registers[R1];
        let y = context.registers[R2];
        let swi = match address {
            0xFFCE => {
                context.registers[R1] = if a as u8 == 0 { y } else { xy_address(x, y)? };
                OS_FIND
            }
            0xFFD4 | 0xFFD7 => {
                context.registers[R1] = y;
                if address == 0xFFD4 { OS_BPUT } else { OS_BGET }
            }
            0xFFE0 => OS_READ_C,
            0xFFE3 => {
                if a as u8 == 13 {
                    OS_NEW_LINE
                } else {
                    OS_WRITE_C
                }
            }
            0xFFE7 => OS_NEW_LINE,
            0xFFEE => OS_WRITE_C,
            0xFFF1 => {
                context.registers[R1] = xy_address(x, y)?;
                OS_WORD
            }
            0xFFF4 => OS_BYTE,
            0xFFF7 => {
                context.registers[R0] = xy_address(x, y)?;
                OS_CLI
            }
            0xFFD1 | 0xFFDA | 0xFFDD => {
                return Err(RuntimeError::Program(format!(
                    "MOS CALL &{address:04X} requires a legacy file control-block adapter not yet implemented"
                )));
            }
            _ => {
                return Err(RuntimeError::Program(format!(
                    "CALL &{address:X} is not a supported MOS entrypoint; arbitrary machine code requires a processor compatibility service"
                )));
            }
        };
        self.dispatch(swi, task, context)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clock_keeps_five_bytes_and_wraps() {
        let clock = MosClock::default();
        clock.set((1 << 32) + 123);
        assert!((1 << 32) <= clock.read());
        clock.set(CLOCK_MASK + 1);
        assert!(clock.read() < 100);
    }

    #[test]
    fn clock_advances_at_centisecond_resolution() {
        let clock = MosClock(Arc::new(ClockState {
            started: Instant::now() - Duration::from_millis(1234),
            offset: AtomicU64::new(10),
        }));
        assert!((133..233).contains(&clock.read()));
    }

    #[test]
    fn split_pointer_overflow_is_an_error() {
        assert!(xy_address(0, 0x1000000).is_err());
        assert_eq!(xy_address(0x20, 0x100).unwrap(), 0x10020);
        assert_eq!(xy_address(0x10020, u32::MAX).unwrap(), 0x10020);
    }
}
