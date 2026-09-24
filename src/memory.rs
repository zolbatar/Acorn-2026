use std::{error::Error, fmt};

use crate::tokenized_basic::TokenizedBasicProgram;

pub const GUEST_MEMORY_BASE: u32 = 0x1000;
pub const GUEST_MEMORY_SIZE: usize = 64 * 1024;

#[derive(Debug)]
pub enum MemoryError {
    AddressOutsideSpace(u32),
    AddressOverflow,
    MissingNullTerminator(u32),
}

impl fmt::Display for MemoryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AddressOutsideSpace(address) => {
                write!(f, "logical address &{address:08X} is outside this task")
            }
            Self::AddressOverflow => write!(f, "logical address range overflowed"),
            Self::MissingNullTerminator(address) => {
                write!(
                    f,
                    "string at logical address &{address:08X} is not terminated"
                )
            }
        }
    }
}

impl Error for MemoryError {}

#[derive(Debug)]
pub struct GuestMemory {
    bytes: Vec<u8>,
}

impl Default for GuestMemory {
    fn default() -> Self {
        Self {
            bytes: vec![0; GUEST_MEMORY_SIZE],
        }
    }
}

impl GuestMemory {
    pub fn read_byte(&self, address: u32) -> Result<u8, MemoryError> {
        let index = self.index(address, 1)?;
        Ok(self.bytes[index])
    }

    pub fn write_byte(&mut self, address: u32, byte: u8) -> Result<(), MemoryError> {
        let index = self.index(address, 1)?;
        self.bytes[index] = byte;
        Ok(())
    }

    pub fn write_bytes(&mut self, address: u32, bytes: &[u8]) -> Result<(), MemoryError> {
        let index = self.index(address, bytes.len())?;
        self.bytes[index..index + bytes.len()].copy_from_slice(bytes);
        Ok(())
    }

    pub fn read_c_string(&self, address: u32, max_bytes: usize) -> Result<Vec<u8>, MemoryError> {
        let mut result = Vec::new();
        for offset in 0..max_bytes {
            let offset = u32::try_from(offset).map_err(|_| MemoryError::AddressOverflow)?;
            let current = address
                .checked_add(offset)
                .ok_or(MemoryError::AddressOverflow)?;
            let byte = self.read_byte(current)?;
            if byte == 0 {
                return Ok(result);
            }
            result.push(byte);
        }
        Err(MemoryError::MissingNullTerminator(address))
    }

    fn index(&self, address: u32, length: usize) -> Result<usize, MemoryError> {
        let Some(offset) = address.checked_sub(GUEST_MEMORY_BASE) else {
            return Err(MemoryError::AddressOutsideSpace(address));
        };
        let start = usize::try_from(offset).map_err(|_| MemoryError::AddressOverflow)?;
        let end = start
            .checked_add(length)
            .ok_or(MemoryError::AddressOverflow)?;
        if end > self.bytes.len() {
            return Err(MemoryError::AddressOutsideSpace(address));
        }
        Ok(start)
    }
}

#[derive(Debug)]
pub struct Task {
    pub id: u64,
    pub memory: GuestMemory,
    pub loaded_tokenized_program: Option<TokenizedBasicProgram>,
}

impl Task {
    pub fn new(id: u64) -> Self {
        Self {
            id,
            memory: GuestMemory::default(),
            loaded_tokenized_program: None,
        }
    }
}
