//! BASIC source and saved-program entry points.
//!
//! Both UTF-8 source and decoded tokenized programs are parsed into the
//! compatibility engine's `ParsedProgram` and executed by its interpreter or
//! optional JIT.

use std::{fs, path::Path};

use crate::{
    basic_compat::{self, JitExecutionReport},
    error::RuntimeError,
    memory::Task,
    swi::SwiDispatcher,
    tokenized_basic::TokenizedBasicProgram,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProgramFormat {
    Basic64Utf8Source,
    ClassicUtf8Source,
    TokenizedBbcProgram,
}

pub fn detect_program_format(path: &Path) -> Result<ProgramFormat, RuntimeError> {
    match path.extension().and_then(|extension| extension.to_str()) {
        Some(extension) if extension.eq_ignore_ascii_case("bas64") => {
            Ok(ProgramFormat::Basic64Utf8Source)
        }
        Some(extension)
            if extension.eq_ignore_ascii_case("bas")
                || extension.eq_ignore_ascii_case("txt")
                || extension.eq_ignore_ascii_case("asc") =>
        {
            Ok(ProgramFormat::ClassicUtf8Source)
        }
        Some(extension) if extension.eq_ignore_ascii_case("bbc") => {
            Ok(ProgramFormat::TokenizedBbcProgram)
        }
        _ => Err(RuntimeError::Program(
            "unsupported file format; use .bas64, .bas, .txt, or .asc for UTF-8 source, or .bbc for a tokenized BASIC program".into(),
        )),
    }
}

pub fn run_file(
    path: &str,
    task: &mut Task,
    dispatcher: &mut SwiDispatcher,
) -> Result<(), RuntimeError> {
    match detect_program_format(Path::new(path))? {
        ProgramFormat::Basic64Utf8Source | ProgramFormat::ClassicUtf8Source => {
            let source = fs::read_to_string(path)?;
            run_source(&source, task, dispatcher)
        }
        ProgramFormat::TokenizedBbcProgram => {
            let program = TokenizedBasicProgram::load_file(path)?;
            basic_compat::run_program(&program, task, dispatcher)
        }
    }
}

pub fn run_file_jit(
    path: &str,
    task: &mut Task,
    dispatcher: &mut SwiDispatcher,
) -> Result<JitExecutionReport, RuntimeError> {
    match detect_program_format(Path::new(path))? {
        ProgramFormat::Basic64Utf8Source | ProgramFormat::ClassicUtf8Source => {
            let source = fs::read_to_string(path)?;
            run_source_jit(&source, task, dispatcher)
        }
        ProgramFormat::TokenizedBbcProgram => {
            let program = TokenizedBasicProgram::load_file(path)?;
            basic_compat::run_program_jit(&program, task, dispatcher)
        }
    }
}

pub fn run_source(
    source: &str,
    task: &mut Task,
    dispatcher: &mut SwiDispatcher,
) -> Result<(), RuntimeError> {
    basic_compat::run_source(source, task, dispatcher)
}

pub fn run_source_jit(
    source: &str,
    task: &mut Task,
    dispatcher: &mut SwiDispatcher,
) -> Result<JitExecutionReport, RuntimeError> {
    basic_compat::run_source_jit(source, task, dispatcher)
}
