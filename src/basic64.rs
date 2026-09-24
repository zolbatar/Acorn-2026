use std::{collections::HashMap, fs, path::Path};

use pest::Parser;
use pest_derive::Parser;

use crate::{
    error::RuntimeError,
    memory::{GUEST_MEMORY_BASE, Task},
    swi::{OS_NEW_LINE, OS_READ_LINE, SwiContext, SwiDispatcher},
};

const INPUT_BUFFER: u32 = GUEST_MEMORY_BASE + 0x2000;
const INPUT_BUFFER_SIZE: u32 = 4096;

#[derive(Parser)]
#[grammar = "basic64.pest"]
struct Basic64Parser;

#[derive(Debug)]
enum Statement {
    Input(String),
    Print(String),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProgramFormat {
    Basic64Utf8Source,
}

pub fn detect_program_format(path: &Path) -> Result<ProgramFormat, RuntimeError> {
    match path.extension().and_then(|extension| extension.to_str()) {
        Some(extension) if extension.eq_ignore_ascii_case("bas64") => {
            Ok(ProgramFormat::Basic64Utf8Source)
        }
        _ => Err(RuntimeError::Program(
            "unsupported file format; native BASIC64 source uses the .bas64 extension".into(),
        )),
    }
}

pub fn run_file(
    path: &str,
    task: &mut Task,
    dispatcher: &mut SwiDispatcher,
) -> Result<(), RuntimeError> {
    let path = Path::new(path);
    match detect_program_format(path)? {
        ProgramFormat::Basic64Utf8Source => {
            let source = fs::read_to_string(path)?;
            run_source(&source, task, dispatcher)
        }
    }
}

pub fn run_source(
    source: &str,
    task: &mut Task,
    dispatcher: &mut SwiDispatcher,
) -> Result<(), RuntimeError> {
    let statements = parse(source)?;
    let mut strings = HashMap::<String, String>::new();

    for statement in statements {
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
                let terminator = INPUT_BUFFER
                    .checked_add(length)
                    .ok_or(crate::memory::MemoryError::AddressOverflow)?;
                task.memory.write_byte(terminator, 0)?;
                let bytes = task
                    .memory
                    .read_c_string(INPUT_BUFFER, INPUT_BUFFER_SIZE as usize)?;
                let value = String::from_utf8(bytes).map_err(|error| {
                    RuntimeError::Program(format!("input is not valid UTF-8: {error}"))
                })?;
                strings.insert(name, value);
            }
            Statement::Print(name) => {
                let value = strings.get(&name).map(String::as_bytes).unwrap_or_default();
                dispatcher.write_indirect(task, value)?;
                dispatcher.dispatch(OS_NEW_LINE, task, &mut SwiContext::default())?;
            }
        }
    }
    Ok(())
}

fn parse(source: &str) -> Result<Vec<Statement>, RuntimeError> {
    let mut statements = Vec::new();
    let parsed = Basic64Parser::parse(Rule::program, source)
        .map_err(|error| RuntimeError::Program(format!("BASIC64 syntax error: {error}")))?;

    for line in parsed.into_iter().flat_map(|program| program.into_inner()) {
        let Some(statement) = line.into_inner().next() else {
            continue;
        };
        let rule = statement.as_rule();
        let variable = statement
            .into_inner()
            .find(|pair| pair.as_rule() == Rule::string_variable)
            .expect("the grammar requires one string variable")
            .as_str()
            .to_ascii_uppercase();

        match rule {
            Rule::input_statement => statements.push(Statement::Input(variable)),
            Rule::print_statement => statements.push(Statement::Print(variable)),
            _ => unreachable!("only input and print statements are in the grammar"),
        }
    }
    Ok(statements)
}
