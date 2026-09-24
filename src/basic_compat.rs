mod parser;
mod runtime;

use crate::{
    error::RuntimeError, memory::Task, swi::SwiDispatcher, tokenized_basic::TokenizedBasicProgram,
};

/// Run a tokenised earlier-version BASIC program in the hosted compatibility
/// personality.
pub fn run_program(
    program: &TokenizedBasicProgram,
    task: &mut Task,
    dispatcher: &mut SwiDispatcher,
) -> Result<(), RuntimeError> {
    let parsed = parser::parse_program(program)?;
    runtime::Interpreter::new(parsed).run(task, dispatcher)
}
