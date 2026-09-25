mod parser;
mod runtime;

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
    let profile = match program.record_layout {
        Some(TokenizedBasicRecordLayout::SharedBoundaryCarriageReturn) => {
            parser::TokenProfile::SharedBoundaryCore
        }
        Some(TokenizedBasicRecordLayout::SeparateLineCarriageReturn) | None => {
            parser::TokenProfile::ArmBasicV
        }
    };

    let parsed = parser::parse_program(program, profile)?;
    runtime::Interpreter::new(parsed).run(task, dispatcher)
}
