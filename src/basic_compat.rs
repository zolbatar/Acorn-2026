#[doc(hidden)]
pub mod compiler_api;

#[cfg(feature = "experimental-jit")]
mod jit;
#[cfg(feature = "experimental-jit")]
mod native_runtime;
mod parser;
mod runtime;
#[cfg(feature = "experimental-jit")]
mod strict_jit;

#[derive(Clone, Debug, Default)]
pub struct JitExecutionReport {
    pub compiled_units: Vec<String>,
    pub compiled_calls: u64,
    pub rendered_pixels: u64,
    /// Executable BASIC statements entered through the reference interpreter.
    pub interpreted_statement_count: u64,
    /// Recursive expression nodes evaluated by the reference interpreter.
    pub interpreted_expression_count: u64,
    /// Checked runtime service/helper calls made by generated native code.
    pub runtime_helper_calls: u64,
    /// True only for a complete strict-native compilation and execution.
    pub strict_native: bool,
    pub compiled_time: std::time::Duration,
    pub compile_time: std::time::Duration,
    pub fallback_reason: Option<String>,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct StrictJitOptions {
    /// Disable speed optimizations to retain loops and procedure calls used
    /// solely for validation measurements.
    pub benchmark_validation: bool,
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

/// Compile the complete parsed program before execution. Unsupported code is
/// rejected with a BASIC source location; strict execution never falls back to
/// the interpreter.
pub fn run_program_jit_strict(
    program: &TokenizedBasicProgram,
    task: &mut Task,
    dispatcher: &mut SwiDispatcher,
) -> Result<JitExecutionReport, RuntimeError> {
    run_program_jit_strict_with_options(program, task, dispatcher, StrictJitOptions::default())
}

pub fn run_program_jit_strict_with_options(
    program: &TokenizedBasicProgram,
    task: &mut Task,
    dispatcher: &mut SwiDispatcher,
    options: StrictJitOptions,
) -> Result<JitExecutionReport, RuntimeError> {
    let parsed = parse_tokenized_program(program)?;
    run_parsed_program_jit_strict(parsed, task, dispatcher, options)
}

pub fn run_source_jit_strict(
    source: &str,
    task: &mut Task,
    dispatcher: &mut SwiDispatcher,
) -> Result<JitExecutionReport, RuntimeError> {
    run_source_jit_strict_with_options(source, task, dispatcher, StrictJitOptions::default())
}

pub fn run_source_jit_strict_with_options(
    source: &str,
    task: &mut Task,
    dispatcher: &mut SwiDispatcher,
    options: StrictJitOptions,
) -> Result<JitExecutionReport, RuntimeError> {
    let parsed = parser::parse_source(source)?;
    run_parsed_program_jit_strict(parsed, task, dispatcher, options)
}

fn run_parsed_program_jit_strict(
    parsed: parser::ParsedProgram,
    task: &mut Task,
    dispatcher: &mut SwiDispatcher,
    options: StrictJitOptions,
) -> Result<JitExecutionReport, RuntimeError> {
    #[cfg(feature = "experimental-jit")]
    {
        dispatcher.set_graphics_profile(parsed.options.target)?;
        return strict_jit::run_parsed_program_with_options(parsed, task, dispatcher, options);
    }

    #[cfg(not(feature = "experimental-jit"))]
    {
        let _ = (parsed, task, dispatcher, options);
        Err(RuntimeError::Program(
            "strict BASICJIT is experimental; start Acorn-2026 with `cargo run-jit` first".into(),
        ))
    }
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
