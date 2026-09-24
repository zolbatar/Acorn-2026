use std::collections::HashMap;

use crate::{
    error::RuntimeError,
    memory::{GUEST_MEMORY_BASE, Task},
    swi::{OS_NEW_LINE, OS_READ_LINE, SwiContext, SwiDispatcher},
    tokenized_basic::{TokenizedBasicLine, TokenizedBasicProgram},
};

const INPUT_BUFFER: u32 = GUEST_MEMORY_BASE + 0x3000;
const INPUT_BUFFER_SIZE: u32 = 4096;

const TOKEN_END: u8 = 0xE0;
const TOKEN_INPUT: u8 = 0xE8;
const TOKEN_PRINT: u8 = 0xF1;
const TOKEN_REM: u8 = 0xF4;

enum Statement {
    Input(String),
    Print(String),
    End,
}

/// Execute the small legacy BASIC subset currently needed to prove that the
/// shared tokenised file format can reach the existing console SWIs.
pub fn run_program(
    program: &TokenizedBasicProgram,
    task: &mut Task,
    dispatcher: &mut SwiDispatcher,
) -> Result<(), RuntimeError> {
    let mut strings = HashMap::<String, Vec<u8>>::new();

    for line in &program.lines {
        let Some(statement) = parse_line(line)? else {
            continue;
        };

        match statement {
            Statement::Input(name) => {
                dispatcher.write_inline(task, b"? ")?;

                let mut input = SwiContext::default();
                input.registers[0] = INPUT_BUFFER;
                input.registers[1] = INPUT_BUFFER_SIZE - 1;
                input.registers[2] = u32::from(b' ');
                input.registers[3] = u32::from(u8::MAX);
                dispatcher.dispatch(OS_READ_LINE, task, &mut input)?;
                if input.carry {
                    return Err(RuntimeError::Program("input interrupted".into()));
                }

                let length = input.registers[1];
                if length >= INPUT_BUFFER_SIZE {
                    return Err(RuntimeError::Program(
                        "OS_ReadLine returned an invalid string length".into(),
                    ));
                }
                let terminator = INPUT_BUFFER
                    .checked_add(length)
                    .ok_or(crate::memory::MemoryError::AddressOverflow)?;
                task.memory.write_byte(terminator, 0)?;
                let value = task
                    .memory
                    .read_c_string(INPUT_BUFFER, INPUT_BUFFER_SIZE as usize)?;
                strings.insert(name, value);
            }
            Statement::Print(name) => {
                let value = strings.get(&name).map(Vec::as_slice).unwrap_or_default();
                dispatcher.write_indirect(task, value)?;
                dispatcher.dispatch(OS_NEW_LINE, task, &mut SwiContext::default())?;
            }
            Statement::End => break,
        }
    }

    Ok(())
}

fn parse_line(line: &TokenizedBasicLine) -> Result<Option<Statement>, RuntimeError> {
    let mut bytes = line.bytes.as_slice();
    while matches!(bytes.first(), Some(b' ' | b'\t')) {
        bytes = &bytes[1..];
    }
    let Some((&token, rest)) = bytes.split_first() else {
        return Ok(None);
    };

    let statement = match token {
        TOKEN_REM => return Ok(None),
        TOKEN_INPUT => Statement::Input(parse_string_variable(rest, line.number)?),
        TOKEN_PRINT => Statement::Print(parse_string_variable(rest, line.number)?),
        TOKEN_END if rest.iter().all(u8::is_ascii_whitespace) => Statement::End,
        TOKEN_END => return unsupported(line.number, "END has trailing tokens"),
        other => {
            return unsupported(
                line.number,
                &format!("token &{other:02X} is outside the current compatibility slice"),
            );
        }
    };

    Ok(Some(statement))
}

fn parse_string_variable(bytes: &[u8], line_number: u16) -> Result<String, RuntimeError> {
    let bytes = trim_ascii_whitespace_start(bytes);
    let Some(first) = bytes.first() else {
        return unsupported(line_number, "expected a string variable ending in $");
    };
    if !first.is_ascii_alphabetic() {
        return unsupported(line_number, "expected a string variable ending in $");
    }

    let mut end = 1;
    while bytes.get(end).is_some_and(u8::is_ascii_alphanumeric) {
        end += 1;
    }
    if bytes.get(end) != Some(&b'$') {
        return unsupported(line_number, "expected a string variable ending in $");
    }
    end += 1;
    if !bytes[end..].iter().all(u8::is_ascii_whitespace) {
        return unsupported(line_number, "only one string variable is supported here");
    }

    Ok(String::from_utf8_lossy(&bytes[..end]).to_ascii_uppercase())
}

fn trim_ascii_whitespace_start(mut bytes: &[u8]) -> &[u8] {
    while matches!(bytes.first(), Some(b' ' | b'\t')) {
        bytes = &bytes[1..];
    }
    bytes
}

fn unsupported<T>(line_number: u16, reason: &str) -> Result<T, RuntimeError> {
    Err(RuntimeError::Program(format!(
        "line {line_number}: {reason}"
    )))
}
