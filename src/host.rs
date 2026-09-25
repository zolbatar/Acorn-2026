use std::{
    io::{self, Read, Write},
    process::{Command, Stdio},
    sync::mpsc::{self, Receiver},
    thread,
};

/// Host terminal access. Guest-visible input and output still pass through SWIs.
pub struct HostConsole {
    input: Receiver<u8>,
    output: Box<dyn Write>,
    _terminal_mode: TerminalMode,
    echo_input: bool,
}

impl HostConsole {
    pub fn stdio() -> Self {
        let terminal_mode = TerminalMode::try_enable_character_input();
        let echo_input = terminal_mode.enabled();
        let (sender, input) = mpsc::channel();
        let _ = thread::Builder::new()
            .name("acorn-stdio-input".into())
            .spawn(move || {
                let stdin = io::stdin();
                let mut stdin = stdin.lock();
                let mut byte = [0_u8; 1];
                loop {
                    match stdin.read(&mut byte) {
                        Ok(0) | Err(_) => break,
                        Ok(_) if sender.send(byte[0]).is_err() => break,
                        Ok(_) => {}
                    }
                }
            });
        Self {
            input,
            output: Box::new(io::stdout()),
            _terminal_mode: terminal_mode,
            echo_input,
        }
    }

    /// In-window console. Input is queued by window events; guest line reads
    /// still consume it through OS_ReadC and OS_ReadLine.
    pub fn windowed(input: Receiver<u8>) -> Self {
        Self {
            input,
            output: Box::new(io::sink()),
            _terminal_mode: TerminalMode { original: None },
            echo_input: true,
        }
    }

    pub fn read_byte(&mut self) -> io::Result<Option<u8>> {
        Ok(self.input.recv().ok())
    }

    pub fn try_read_byte(&self) -> Option<u8> {
        self.input.try_recv().ok()
    }

    pub fn write_byte(&mut self, byte: u8) -> io::Result<()> {
        self.output.write_all(&[byte])
    }

    pub fn flush(&mut self) -> io::Result<()> {
        self.output.flush()
    }

    /// Raw terminal input needs software echo; a cooked terminal echoes itself.
    pub fn software_echo(&self) -> bool {
        self.echo_input
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
