use std::{collections::BTreeMap, error::Error, fmt};

use crate::filesystem::OpenFile;
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
            // Keep the compatibility zero page task-local. Legacy BASIC may
            // use zero-valued variables with an indirection operator during
            // initialisation, while managed allocations still begin at
            // GUEST_MEMORY_BASE.
            bytes: vec![0; GUEST_MEMORY_BASE as usize + GUEST_MEMORY_SIZE],
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
        let start = usize::try_from(address).map_err(|_| MemoryError::AddressOverflow)?;
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
    pub file_system: FileSystemContext,
}

#[derive(Debug)]
pub struct FileSystemContext {
    pub current_file_system: String,
    pub temporary_file_system: String,
    pub current_directory: Vec<String>,
    pub user_root: Vec<String>,
    pub library_directory: Vec<String>,
    pub previous_directory: Vec<String>,
    pub open_files: BTreeMap<u32, OpenFile>,
    next_handle: u32,
}

impl Default for FileSystemContext {
    fn default() -> Self {
        Self {
            current_file_system: "HostFS".to_string(),
            temporary_file_system: "HostFS".to_string(),
            current_directory: Vec::new(),
            user_root: Vec::new(),
            library_directory: Vec::new(),
            previous_directory: Vec::new(),
            open_files: BTreeMap::new(),
            next_handle: 1,
        }
    }
}

impl FileSystemContext {
    pub fn insert_file(&mut self, open_file: OpenFile) -> u32 {
        let handle = self.next_handle.max(1);
        self.next_handle = handle.wrapping_add(1).max(1);
        self.open_files.insert(handle, open_file);
        handle
    }
}

impl Task {
    pub fn new(id: u64) -> Self {
        Self {
            id,
            memory: GuestMemory::default(),
            loaded_tokenized_program: None,
            file_system: FileSystemContext::default(),
        }
    }
}
