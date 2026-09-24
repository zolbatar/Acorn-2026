use std::{error::Error, fmt, io};

use crate::memory::MemoryError;

#[derive(Debug)]
pub enum RuntimeError {
    EndOfInput,
    InvalidSwi(u32),
    Io(io::Error),
    Memory(MemoryError),
    Program(String),
}

impl fmt::Display for RuntimeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EndOfInput => write!(f, "end of input"),
            Self::InvalidSwi(number) => write!(f, "unsupported SWI &{number:02X}"),
            Self::Io(error) => write!(f, "host I/O error: {error}"),
            Self::Memory(error) => write!(f, "guest memory error: {error}"),
            Self::Program(message) => write!(f, "{message}"),
        }
    }
}

impl Error for RuntimeError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Memory(error) => Some(error),
            Self::EndOfInput | Self::InvalidSwi(_) | Self::Program(_) => None,
        }
    }
}

impl From<io::Error> for RuntimeError {
    fn from(value: io::Error) -> Self {
        Self::Io(value)
    }
}

impl From<MemoryError> for RuntimeError {
    fn from(value: MemoryError) -> Self {
        Self::Memory(value)
    }
}
