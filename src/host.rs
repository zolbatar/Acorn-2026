use std::{
    io::{self, Read, Write},
    process::{Command, Stdio},
};

/// Host terminal access. Guest-visible input and output still pass through SWIs.
pub struct HostConsole {
    input: Box<dyn Read>,
    output: Box<dyn Write>,
    terminal_mode: TerminalMode,
}

impl HostConsole {
    pub fn stdio() -> Self {
        let terminal_mode = TerminalMode::try_enable_character_input();
        Self {
            input: Box::new(io::stdin()),
            output: Box::new(io::stdout()),
            terminal_mode,
        }
    }

    pub fn read_byte(&mut self) -> io::Result<Option<u8>> {
        let mut byte = [0_u8; 1];
        match self.input.read(&mut byte)? {
            0 => Ok(None),
            _ => Ok(Some(byte[0])),
        }
    }

    pub fn write_byte(&mut self, byte: u8) -> io::Result<()> {
        self.output.write_all(&[byte])
    }

    pub fn flush(&mut self) -> io::Result<()> {
        self.output.flush()
    }

    /// Raw terminal input needs software echo; a cooked terminal echoes itself.
    pub fn software_echo(&self) -> bool {
        self.terminal_mode.enabled()
    }
}

/// Temporarily disables terminal echo and canonical buffering so OS_ReadLine
/// can provide its character-by-character behavior through OS_WriteC.
struct TerminalMode {
    original: Option<String>,
}

impl TerminalMode {
    fn try_enable_character_input() -> Self {
        #[cfg(unix)]
        {
            let Ok(state) = Command::new("stty")
                .arg("-g")
                .stdin(Stdio::inherit())
                .output()
            else {
                return Self { original: None };
            };
            if !state.status.success() {
                return Self { original: None };
            }
            let original = String::from_utf8_lossy(&state.stdout).trim().to_owned();
            if original.is_empty() {
                return Self { original: None };
            }

            let Ok(status) = Command::new("stty")
                .args(["-echo", "-icanon", "min", "1", "time", "0"])
                .stdin(Stdio::inherit())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
            else {
                return Self { original: None };
            };
            if status.success() {
                Self {
                    original: Some(original),
                }
            } else {
                Self { original: None }
            }
        }

        #[cfg(not(unix))]
        {
            Self { original: None }
        }
    }

    fn enabled(&self) -> bool {
        self.original.is_some()
    }
}

impl Drop for TerminalMode {
    fn drop(&mut self) {
        #[cfg(unix)]
        if let Some(original) = self.original.take() {
            let _ = Command::new("stty")
                .arg(original)
                .stdin(Stdio::inherit())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        }
    }
}
