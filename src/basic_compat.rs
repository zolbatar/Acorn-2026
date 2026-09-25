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
    if program.record_layout == Some(TokenizedBasicRecordLayout::SharedBoundaryCarriageReturn) {
        return Err(RuntimeError::Program(
            "BASICRUN can load this saved-program layout, but execution currently supports the ARM BASIC V token profile only".into(),
        ));
    }

    let parsed = parser::parse_program(program)?;
    runtime::Interpreter::new(parsed).run(task, dispatcher)
}
