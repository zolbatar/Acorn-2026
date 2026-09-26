#[doc(hidden)]
pub mod compiler_api;

#[cfg(feature = "experimental-jit")]
mod jit;
mod parser;
mod runtime;

#[derive(Clone, Debug, Default)]
pub struct JitExecutionReport {
    pub compiled_units: Vec<String>,
    pub compiled_calls: u64,
    pub rendered_pixels: u64,
    pub compiled_time: std::time::Duration,
    pub compile_time: std::time::Duration,
    pub fallback_reason: Option<String>,
}

use crate::{
    error::RuntimeError,
    memory::Task,
    swi::SwiDispatcher,
    tokenized_basic::{TokenizedBasicProgram, TokenizedBasicRecordLayout},
};

/// Run a tokenised earlier-version BASIC program in the hosted compatibility
/// personality.
pub fn run_program(
    program: &TokenizedBasicProgram,
    task: &mut Task,
    dispatcher: &mut SwiDispatcher,
) -> Result<(), RuntimeError> {
    let parsed = parse_tokenized_program(program)?;
    run_parsed_program(parsed, task, dispatcher)
}

/// Run plain-text BASIC source through the same parser output and interpreter
/// used for decoded tokenized programs.
pub fn run_source(
    source: &str,
    task: &mut Task,
    dispatcher: &mut SwiDispatcher,
) -> Result<(), RuntimeError> {
    let parsed = parser::parse_source(source)?;
    run_parsed_program(parsed, task, dispatcher)
}

fn parse_tokenized_program(
    program: &TokenizedBasicProgram,
) -> Result<parser::ParsedProgram, RuntimeError> {
    let profile = match program.record_layout {
        Some(TokenizedBasicRecordLayout::SharedBoundaryCarriageReturn) => {
            parser::TokenProfile::SharedBoundaryCore
        }
        Some(TokenizedBasicRecordLayout::SeparateLineCarriageReturn) | None => {
            parser::TokenProfile::ArmBasicV
        }
    };
    parser::parse_program(program, profile)
}

fn run_parsed_program(
    parsed: parser::ParsedProgram,
    task: &mut Task,
    dispatcher: &mut SwiDispatcher,
) -> Result<(), RuntimeError> {
    dispatcher.set_graphics_profile(parsed.options.target)?;
    runtime::Interpreter::new(parsed).run(task, dispatcher)
}

/// Run a tokenised BASIC program with the experimental hybrid Cranelift path.
/// Eligible demo kernels compile and run natively; every other statement
/// continues through the compatibility interpreter.
pub fn run_program_jit(
    program: &TokenizedBasicProgram,
    task: &mut Task,
    dispatcher: &mut SwiDispatcher,
) -> Result<JitExecutionReport, RuntimeError> {
    let parsed = parse_tokenized_program(program)?;
    run_parsed_program_jit(parsed, task, dispatcher)
}

pub fn run_source_jit(
    source: &str,
    task: &mut Task,
    dispatcher: &mut SwiDispatcher,
) -> Result<JitExecutionReport, RuntimeError> {
    let parsed = parser::parse_source(source)?;
    run_parsed_program_jit(parsed, task, dispatcher)
}

fn run_parsed_program_jit(
    parsed: parser::ParsedProgram,
    task: &mut Task,
    dispatcher: &mut SwiDispatcher,
) -> Result<JitExecutionReport, RuntimeError> {
    #[cfg(feature = "experimental-jit")]
    {
        dispatcher.set_graphics_profile(parsed.options.target)?;
        return jit::run_parsed_program_jit(parsed, task, dispatcher);
    }

    #[cfg(not(feature = "experimental-jit"))]
    {
        let _ = (parsed, task, dispatcher);
        Err(RuntimeError::Program(
            "BASICJIT is experimental; start Acorn-2026 with `cargo run-jit` first".into(),
        ))
    }
}
