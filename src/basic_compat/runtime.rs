use std::{collections::HashMap, time::Instant};

use crate::{
    error::RuntimeError,
    memory::{GUEST_MEMORY_BASE, GUEST_MEMORY_SIZE, Task},
    swi::{OS_NEW_LINE, OS_READ_LINE, SwiContext, SwiDispatcher},
};

#[cfg(feature = "experimental-jit")]
use super::jit::JitProgram;
use super::parser::{
    BinaryOp, DimDeclaration, Expr, LValue, MemoryWidth, ParsedProgram, PrintItem, Statement,
    UnaryOp, VduFormat,
};

const INPUT_BUFFER: u32 = GUEST_MEMORY_BASE + 0x3000;
const INPUT_BUFFER_SIZE: u32 = 4096;
const FIRST_HEAP_ADDRESS: u32 = GUEST_MEMORY_BASE + 0x8000;
// The source-derived full Mandelbrot fixture can execute several billion
// statements with its original dimensions and iteration cap.
const MAX_EXECUTION_STEPS: u64 = 100_000_000_000;
#[cfg(feature = "experimental-jit")]
pub(super) const MAX_NATIVE_PROCEDURE_CALLS: u64 = 1_000_000;
const PRINT_ZONE_WIDTH: usize = 14;
const DEFAULT_PRINT_FORMAT: u32 = 0x0000_090A;

#[derive(Clone, Debug, PartialEq)]
enum Value {
    Number(f64),
    String(Vec<u8>),
}

impl Value {
    fn number(&self, line: u16) -> Result<f64, RuntimeError> {
        match self {
            Self::Number(value) => Ok(*value),
            Self::String(_) => Err(program_error(line, "expected a numeric expression")),
        }
    }

    fn string(&self, line: u16) -> Result<&[u8], RuntimeError> {
        match self {
            Self::String(value) => Ok(value),
            Self::Number(_) => Err(program_error(line, "expected a string expression")),
        }
    }
}

#[derive(Clone, Copy)]
enum ReturnKind {
    Procedure,
    Subroutine,
}

#[derive(Clone)]
struct ReturnFrame {
    kind: ReturnKind,
    address: usize,
    saved_variables: Vec<(String, Option<Value>)>,
}

struct ForFrame {
    variable: String,
    limit: f64,
    step: f64,
    body_address: usize,
    next_address: usize,
}

enum Flow {
    Next,
    Jump(usize),
    Stop,
}

pub(super) struct Interpreter {
    program: ParsedProgram,
    variables: HashMap<String, Value>,
    arrays: HashMap<String, Vec<Value>>,
    data: Vec<(u16, Expr)>,
    data_cursor: usize,
    returns: Vec<ReturnFrame>,
    for_loops: Vec<ForFrame>,
    repeat_loops: Vec<usize>,
    for_pairs: HashMap<usize, usize>,
    if_blocks: HashMap<usize, usize>,
    started: Instant,
    steps: u64,
    print_column: usize,
    print_format: u32,
    next_heap_address: u32,
    pending_key: Option<u8>,
    random_state: u32,
    last_random_fraction: f64,
    #[cfg(feature = "experimental-jit")]
    jit: Option<JitProgram>,
    #[cfg(feature = "experimental-jit")]
    jit_fallback: Option<String>,
}

/// Synchronous callback state shared with a compiled numeric procedure.
/// The JIT receives only this opaque pointer and calls the checked helpers
/// below; it never receives a host pointer into guest memory.
#[cfg(feature = "experimental-jit")]
pub(super) struct NativeProcedureContext {
    task: *mut Task,
    dispatcher: *mut SwiDispatcher,
    variables: *mut HashMap<String, Value>,
    variable_names: *const String,
    variable_count: usize,
    steps: *mut u64,
    pending_key: *mut Option<u8>,
    line: u16,
    calls: u64,
    error: Option<RuntimeError>,
}

#[cfg(feature = "experimental-jit")]
impl NativeProcedureContext {
    fn new(
        variables: &mut HashMap<String, Value>,
        task: &mut Task,
        dispatcher: &mut SwiDispatcher,
        variable_names: &[String],
        steps: &mut u64,
        pending_key: &mut Option<u8>,
        line: u16,
    ) -> Self {
        Self {
            task,
            dispatcher,
            variables,
            variable_names: variable_names.as_ptr(),
            variable_count: variable_names.len(),
            steps,
            pending_key,
            line,
            calls: 0,
            error: None,
        }
    }

    pub(super) fn call_count(&self) -> u64 {
        self.calls
    }

    pub(super) fn take_error(&mut self) -> Option<RuntimeError> {
        self.error.take()
    }
}

#[cfg(feature = "experimental-jit")]
pub(super) extern "C" fn native_procedure_enter(context: *mut std::ffi::c_void) -> i32 {
    if context.is_null() {
        return 0;
    }
    // SAFETY: the opaque pointer is created by `NativeProcedureContext::new`
    // and remains alive throughout the synchronous native procedure call.
    let context = unsafe { &mut *context.cast::<NativeProcedureContext>() };
    if context.error.is_some() {
        return 0;
    }
    context.calls = context.calls.saturating_add(1);
    if context.calls > MAX_NATIVE_PROCEDURE_CALLS {
        context.error = Some(program_error(
            context.line,
            "native procedure call budget exceeded",
        ));
        0
    } else {
        1
    }
}

#[cfg(feature = "experimental-jit")]
pub(super) extern "C" fn native_procedure_tick(context: *mut std::ffi::c_void, line: i32) -> i32 {
    if context.is_null() {
        return 0;
    }
    // SAFETY: the opaque pointer is created by `NativeProcedureContext::new`
    // and remains alive throughout the synchronous native procedure call.
    let context = unsafe { &mut *context.cast::<NativeProcedureContext>() };
    if context.error.is_some() {
        return 0;
    }
    context.line = line.clamp(0, i32::from(u16::MAX)) as u16;

    // SAFETY: the interpreter lends this counter to the context exclusively
    // until the compiled procedure returns.
    let steps = unsafe { &mut *context.steps };
    *steps = steps.saturating_add(1);
    if *steps > MAX_EXECUTION_STEPS {
        context.error = Some(program_error(context.line, "execution step limit reached"));
        return 0;
    }
    if *steps & 0x3FF == 0 {
        // SAFETY: both pointers remain exclusively borrowed for this call.
        let dispatcher = unsafe { &mut *context.dispatcher };
        if let Some(key) = dispatcher.poll_key() {
            unsafe { *context.pending_key = Some(key) };
        }
    }
    1
}

#[cfg(feature = "experimental-jit")]
pub(super) extern "C" fn native_procedure_context_ok(context: *mut std::ffi::c_void) -> i32 {
    if context.is_null() {
        return 0;
    }
    // SAFETY: the opaque pointer is created by `NativeProcedureContext::new`
    // and remains alive throughout the synchronous native procedure call.
    i32::from(unsafe { (*context.cast::<NativeProcedureContext>()).error.is_none() })
}

#[cfg(feature = "experimental-jit")]
pub(super) extern "C" fn native_procedure_get_variable(
    context: *mut std::ffi::c_void,
    index: i32,
) -> f64 {
    if context.is_null() {
        return 0.0;
    }
    // SAFETY: the opaque pointer is created by `NativeProcedureContext::new`
    // and remains alive throughout the synchronous native procedure call.
    let context = unsafe { &mut *context.cast::<NativeProcedureContext>() };
    if context.error.is_some() || index < 0 || index as usize >= context.variable_count {
        if context.error.is_none() {
            context.error = Some(program_error(
                context.line,
                "compiled procedure referenced an invalid variable slot",
            ));
        }
        return 0.0;
    }

    // SAFETY: the names slice and variable map are exclusively borrowed for
    // the duration of this synchronous native procedure call.
    let name = unsafe { &*context.variable_names.add(index as usize) };
    let variables = unsafe { &*context.variables };
    match variables
        .get(name)
        .cloned()
        .unwrap_or_else(|| default_value(name))
        .number(context.line)
    {
        Ok(value) => value,
        Err(error) => {
            context.error = Some(error);
            0.0
        }
    }
}

#[cfg(feature = "experimental-jit")]
pub(super) extern "C" fn native_procedure_set_variable(
    context: *mut std::ffi::c_void,
    index: i32,
    value: f64,
) -> i32 {
    if context.is_null() {
        return 0;
    }
    // SAFETY: the opaque pointer is created by `NativeProcedureContext::new`
    // and remains alive throughout the synchronous native procedure call.
    let context = unsafe { &mut *context.cast::<NativeProcedureContext>() };
    if context.error.is_some() || index < 0 || index as usize >= context.variable_count {
        if context.error.is_none() {
            context.error = Some(program_error(
                context.line,
                "compiled procedure referenced an invalid variable slot",
            ));
        }
        return 0;
    }

    // SAFETY: the names slice and variable map are exclusively borrowed for
    // the duration of this synchronous native procedure call.
    let name = unsafe { &*context.variable_names.add(index as usize) };
    let value = if name.ends_with('%') {
        f64::from(value.trunc() as i32)
    } else {
        value
    };
    unsafe {
        (&mut *context.variables).insert(name.clone(), Value::Number(value));
    }
    1
}

#[cfg(feature = "experimental-jit")]
pub(super) extern "C" fn native_procedure_graphics(
    context: *mut std::ffi::c_void,
    operation: i32,
    first: f64,
    second: f64,
) -> i32 {
    if context.is_null() {
        return 0;
    }
    // SAFETY: the opaque pointer is created by `NativeProcedureContext::new`
    // and remains alive throughout the synchronous native procedure call.
    let context = unsafe { &mut *context.cast::<NativeProcedureContext>() };
    if context.error.is_some() {
        return 0;
    }

    let result = (|| {
        // SAFETY: these references are exclusively borrowed for the duration
        // of this synchronous native procedure call.
        let task = unsafe { &mut *context.task };
        let dispatcher = unsafe { &mut *context.dispatcher };
        match operation {
            0 => dispatcher
                .write_via_os_write_c(task, &[18, first.trunc() as u8, second.trunc() as u8]),
            1 | 2 => dispatcher.plot(
                task,
                if operation == 1 { 4 } else { 5 },
                graphics_coordinate(first, context.line)?,
                graphics_coordinate(second, context.line)?,
            ),
            _ => Err(program_error(
                context.line,
                "compiled procedure requested an invalid graphics operation",
            )),
        }
    })();
    match result {
        Ok(()) => 1,
        Err(error) => {
            context.error = Some(error);
            0
        }
    }
}

#[cfg(feature = "experimental-jit")]
pub(super) extern "C" fn native_procedure_integer(value: f64) -> f64 {
    f64::from(value.trunc() as i32)
}

impl Interpreter {
    pub(super) fn new(program: ParsedProgram) -> Self {
        let data = program
            .instructions
            .iter()
            .filter_map(|instruction| match &instruction.statement {
                Statement::Data(values) => Some(
                    values
                        .iter()
                        .cloned()
                        .map(|value| (instruction.line_number, value)),
                ),
                _ => None,
            })
            .flatten()
            .collect();
        let for_pairs = match_for_loops(&program);
        let if_blocks = match_if_blocks(&program);

        Self {
            program,
            variables: HashMap::new(),
            arrays: HashMap::new(),
            data,
            data_cursor: 0,
            returns: Vec::new(),
            for_loops: Vec::new(),
            repeat_loops: Vec::new(),
            for_pairs,
            if_blocks,
            started: Instant::now(),
            steps: 0,
            print_column: 0,
            print_format: DEFAULT_PRINT_FORMAT,
            next_heap_address: FIRST_HEAP_ADDRESS,
            pending_key: None,
            random_state: 0xA341_316C,
            last_random_fraction: 0.0,
            #[cfg(feature = "experimental-jit")]
            jit: None,
            #[cfg(feature = "experimental-jit")]
            jit_fallback: None,
        }
    }

    #[cfg(feature = "experimental-jit")]
    pub(super) fn install_jit(&mut self, jit: JitProgram) {
        self.jit = Some(jit);
    }

    #[cfg(feature = "experimental-jit")]
    pub(super) fn set_jit_fallback(&mut self, reason: &str) {
        self.jit_fallback = Some(reason.to_string());
    }

    #[cfg(feature = "experimental-jit")]
    pub(super) fn jit_report(&self) -> super::JitExecutionReport {
        let mut report = self
            .jit
            .as_ref()
            .map(JitProgram::report)
            .unwrap_or_default();
        report.fallback_reason = self
            .jit_fallback
            .clone()
            .or(report.fallback_reason)
            .or_else(|| {
                if report.compiled_units.is_empty() {
                    Some("no verified native regions matched; the entire program was interpreted".into())
                } else {
                    Some("BASIC control flow, graphics, SWIs, and unmatched statements used the interpreter".into())
                }
            });
        report
    }

    pub(super) fn run(
        &mut self,
        task: &mut Task,
        dispatcher: &mut SwiDispatcher,
    ) -> Result<(), RuntimeError> {
        let mut address = 0_usize;
        while address < self.program.instructions.len() {
            self.steps += 1;
            if self.steps & 0x3FF == 0 {
                if let Some(key) = dispatcher.poll_key() {
                    self.pending_key = Some(key);
                }
            }
            if self.steps > MAX_EXECUTION_STEPS {
                let line = self.program.instructions[address].line_number;
                return Err(program_error(line, "execution step limit reached"));
            }

            #[cfg(feature = "experimental-jit")]
            if self
                .jit
                .as_ref()
                .and_then(JitProgram::mandelbrot_frame_start)
                == Some(address)
            {
                let line = self.program.instructions[address].line_number;
                let inputs = super::jit::MandelbrotFrameInputs {
                    width: self.integer_variable("XSIZE%", line)?,
                    height: self.integer_variable("YSIZE%", line)?,
                    x_width: self.get_variable("XWIDTH").number(line)?,
                    x_min: self.get_variable("XMIN").number(line)?,
                    y_width: self.get_variable("YWIDTH").number(line)?,
                    y_min: self.get_variable("YMIN").number(line)?,
                    iteration_limit: self.integer_variable("MAX%", line)?,
                };
                let initial_rgb = ["R%", "G%", "B%"]
                    .map(|name| self.integer_variable(name, line))
                    .into_iter()
                    .collect::<Result<Vec<_>, _>>()?
                    .try_into()
                    .expect("three initial RGB components");
                let frame = if let Some(jit) = self.jit.as_mut() {
                    jit.run_mandelbrot_frame(
                        inputs,
                        task,
                        dispatcher,
                        &mut self.pending_key,
                        initial_rgb,
                    )?
                } else {
                    None
                };
                if let Some(frame) = frame {
                    self.set_variable("X%", Value::Number(f64::from(frame.width)), line)?;
                    self.set_variable("Y%", Value::Number(f64::from(frame.height)), line)?;
                    self.set_variable("A", Value::Number(frame.last_pixel.real_c), line)?;
                    self.set_variable("B", Value::Number(frame.last_pixel.imag_c), line)?;
                    self.set_variable(
                        "IT%",
                        Value::Number(f64::from(frame.last_pixel.iterations)),
                        line,
                    )?;
                    self.set_variable("E", Value::Number(frame.last_pixel.real_z), line)?;
                    self.set_variable("F", Value::Number(frame.last_pixel.imag_z), line)?;
                    self.set_variable("U", Value::Number(frame.last_pixel.u), line)?;
                    self.set_variable("V", Value::Number(frame.last_pixel.v), line)?;
                    self.set_variable("H%", Value::Number(f64::from(frame.last_pixel.hue)), line)?;
                    for (name, value) in ["R%", "G%", "B%"].into_iter().zip(frame.rgb) {
                        self.set_variable(name, Value::Number(f64::from(value)), line)?;
                    }
                    address = frame.after;
                    continue;
                }
            }

            #[cfg(feature = "experimental-jit")]
            if self
                .jit
                .as_ref()
                .and_then(JitProgram::mandelbrot_inline_start)
                == Some(address)
            {
                let line = self.program.instructions[address].line_number;
                let real_c = self.get_variable("A").number(line)?;
                let imag_c = self.get_variable("B").number(line)?;
                let iteration_limit = self.integer_variable("MAX%", line)?;
                let Some((after, iterations, [real_z, imag_z, u, v])) = self
                    .jit
                    .as_mut()
                    .and_then(|jit| jit.run_mandelbrot_inline(real_c, imag_c, iteration_limit))
                else {
                    address += 1;
                    continue;
                };
                self.set_variable("IT%", Value::Number(f64::from(iterations)), line)?;
                self.set_variable("E", Value::Number(real_z), line)?;
                self.set_variable("F", Value::Number(imag_z), line)?;
                self.set_variable("U", Value::Number(u), line)?;
                self.set_variable("V", Value::Number(v), line)?;
                address = after;
                continue;
            }

            #[cfg(feature = "experimental-jit")]
            if self
                .jit
                .as_ref()
                .and_then(JitProgram::clocksp5_region_start)
                == Some(address)
            {
                let line = self.program.instructions[address].line_number;
                let values = ["B%", "L%", "I%", "D%", "E%"]
                    .map(|name| self.integer_variable(name, line))
                    .into_iter()
                    .collect::<Result<Vec<_>, _>>()?;
                let values: [i32; 5] = values.try_into().expect("five integer inputs");
                let Some((after, final_l, final_c)) = self
                    .jit
                    .as_mut()
                    .and_then(|jit| jit.run_clocksp5_integer_region(values))
                else {
                    // A vanished entry is impossible while the owned JIT
                    // module is alive; keep the interpreter as a safe fallback.
                    address += 1;
                    continue;
                };
                self.set_variable("L%", Value::Number(f64::from(final_l)), line)?;
                self.set_variable("C%", Value::Number(f64::from(final_c)), line)?;
                address = after;
                continue;
            }

            #[cfg(feature = "experimental-jit")]
            if let Statement::ProcedureCall(name, arguments) =
                self.program.instructions[address].statement.clone()
            {
                if let Some(variable_names) = self
                    .jit
                    .as_ref()
                    .and_then(|jit| jit.native_procedure_variables(&name))
                {
                    let line = self.program.instructions[address].line_number;
                    let values = self.evaluate_arguments(&arguments, line, task)?;
                    let expected_arguments = self
                        .program
                        .procedures
                        .get(&name)
                        .map(|definition| definition.parameters.len());
                    let numeric_values = if expected_arguments == Some(values.len()) {
                        Some(
                            values
                                .iter()
                                .map(|value| value.number(line))
                                .collect::<Result<Vec<_>, _>>()?,
                        )
                    } else {
                        None
                    };
                    let can_run_natively = numeric_values.as_ref().is_some_and(|inputs| {
                        self.jit
                            .as_ref()
                            .is_some_and(|jit| jit.native_procedure_is_safe(&name, inputs))
                    });
                    if can_run_natively {
                        let numeric_values = numeric_values.as_ref().expect("checked above");
                        let mut context = NativeProcedureContext::new(
                            &mut self.variables,
                            task,
                            dispatcher,
                            &variable_names,
                            &mut self.steps,
                            &mut self.pending_key,
                            line,
                        );
                        let status = match self.jit.as_mut() {
                            Some(jit) => {
                                jit.run_native_procedure(&name, &mut context, numeric_values)?
                            }
                            None => false,
                        };
                        if status {
                            address += 1;
                            continue;
                        }
                    }

                    match self.execute_procedure_call_values(&name, values, address, line)? {
                        Flow::Next => address += 1,
                        Flow::Jump(destination) => address = destination,
                        Flow::Stop => break,
                    }
                    continue;
                }
            }

            #[cfg(feature = "experimental-jit")]
            if let Some(variables) = self
                .jit
                .as_ref()
                .and_then(|jit| jit.numeric_statement_variables(address))
            {
                let line = self.program.instructions[address].line_number;
                let inputs = variables
                    .iter()
                    .map(|name| self.get_variable(name).number(line))
                    .collect::<Result<Vec<_>, _>>()?;
                let outputs = self
                    .jit
                    .as_mut()
                    .and_then(|jit| jit.run_numeric_statement(address, &inputs));
                if let Some(outputs) = outputs {
                    let instruction = self.program.instructions[address].clone();
                    let statement = super::jit::numeric_statement_with_results(
                        &instruction.statement,
                        &outputs,
                    )
                    .ok_or_else(|| {
                        program_error(line, "compiled numeric statement no longer matches")
                    })?;
                    match self.execute_statement(
                        &statement,
                        address,
                        instruction.line_number,
                        task,
                        dispatcher,
                    )? {
                        Flow::Next => address += 1,
                        Flow::Jump(destination) => address = destination,
                        Flow::Stop => break,
                    }
                    continue;
                }
            }

            let instruction = self.program.instructions[address].clone();
            match self.execute_statement(
                &instruction.statement,
                address,
                instruction.line_number,
                task,
                dispatcher,
            )? {
                Flow::Next => address += 1,
                Flow::Jump(destination) => address = destination,
                Flow::Stop => break,
            }
        }
        Ok(())
    }

    fn execute_statement(
        &mut self,
        statement: &Statement,
        address: usize,
        line: u16,
        task: &mut Task,
        dispatcher: &mut SwiDispatcher,
    ) -> Result<Flow, RuntimeError> {
        match statement {
            Statement::Assign(target, expression) => {
                let value = self.evaluate(expression, line, task)?;
                self.assign(target, value, line, task)?;
                Ok(Flow::Next)
            }
            Statement::Input(target) => {
                let value = self.read_input(line, task, dispatcher)?;
                self.assign(target, value, line, task)?;
                Ok(Flow::Next)
            }
            Statement::Print(items) => {
                self.print(items, line, task, dispatcher)?;
                Ok(Flow::Next)
            }
            Statement::ClearScreen => {
                dispatcher.write_via_os_write_c(task, &[12])?;
                Ok(Flow::Next)
            }
            Statement::ClearGraphics => {
                dispatcher.write_via_os_write_c(task, &[16])?;
                Ok(Flow::Next)
            }
            Statement::Colour(colours) => {
                if colours.is_empty() || colours.len() > 2 {
                    return Err(program_error(line, "COLOUR expects one or two values"));
                }
                let foreground = self.evaluate(&colours[0], line, task)?.number(line)?;
                dispatcher.write_via_os_write_c(task, &[17, foreground.trunc() as u8])?;
                if let Some(background) = colours.get(1) {
                    let background = self.evaluate(background, line, task)?.number(line)?;
                    dispatcher
                        .write_via_os_write_c(task, &[17, (background.trunc() as u8) | 0x80])?;
                }
                Ok(Flow::Next)
            }
            Statement::PrintFormat(expression) => {
                let format = self.evaluate(expression, line, task)?.number(line)?.trunc() as i64;
                self.print_format = if format == 0 {
                    DEFAULT_PRINT_FORMAT
                } else {
                    format as u32
                };
                Ok(Flow::Next)
            }
            Statement::Mode(expression) => {
                let mode = self.evaluate(expression, line, task)?.number(line)?.trunc();
                if mode >= f64::from(GUEST_MEMORY_BASE) {
                    dispatcher.set_mode_from_block(task, checked_guest_address(mode, line)?)?;
                } else {
                    dispatcher.write_via_os_write_c(task, &[22, mode as u8])?;
                }
                Ok(Flow::Next)
            }
            Statement::Vdu(arguments) => {
                let mut bytes = Vec::new();
                for argument in arguments {
                    let value = self
                        .evaluate(&argument.value, line, task)?
                        .number(line)?
                        .trunc() as i64;
                    match argument.format {
                        VduFormat::Byte => bytes.push(value as u8),
                        VduFormat::Word => bytes.extend_from_slice(&(value as u16).to_le_bytes()),
                        VduFormat::Padded => {
                            bytes.push(value as u8);
                            bytes.extend_from_slice(&[0; 9]);
                        }
                    }
                }
                dispatcher.write_via_os_write_c(task, &bytes)?;
                Ok(Flow::Next)
            }
            Statement::Line(x1, y1, x2, y2) => {
                let x1 = self.evaluate(x1, line, task)?.number(line)?;
                let y1 = self.evaluate(y1, line, task)?.number(line)?;
                let x2 = self.evaluate(x2, line, task)?.number(line)?;
                let y2 = self.evaluate(y2, line, task)?.number(line)?;
                dispatcher.plot(
                    task,
                    4,
                    graphics_coordinate(x1, line)?,
                    graphics_coordinate(y1, line)?,
                )?;
                dispatcher.plot(
                    task,
                    5,
                    graphics_coordinate(x2, line)?,
                    graphics_coordinate(y2, line)?,
                )?;
                Ok(Flow::Next)
            }
            Statement::Move(x, y) | Statement::Draw(x, y) => {
                let plot_code = if matches!(statement, Statement::Move(_, _)) {
                    4
                } else {
                    5
                };
                let x = self.evaluate(x, line, task)?.number(line)?;
                let y = self.evaluate(y, line, task)?.number(line)?;
                dispatcher.plot(
                    task,
                    plot_code,
                    graphics_coordinate(x, line)?,
                    graphics_coordinate(y, line)?,
                )?;
                Ok(Flow::Next)
            }
            Statement::Plot(code, x, y) => {
                let code = self.evaluate(code, line, task)?.number(line)?.trunc() as u8;
                let x = self.evaluate(x, line, task)?.number(line)?;
                let y = self.evaluate(y, line, task)?.number(line)?;
                dispatcher.plot(
                    task,
                    code,
                    graphics_coordinate(x, line)?,
                    graphics_coordinate(y, line)?,
                )?;
                Ok(Flow::Next)
            }
            Statement::Gcol(action, colour) => {
                let action = self.evaluate(action, line, task)?.number(line)?.trunc() as u8;
                let colour = self.evaluate(colour, line, task)?.number(line)?.trunc() as u8;
                dispatcher.write_via_os_write_c(task, &[18, action, colour])?;
                Ok(Flow::Next)
            }
            Statement::If(condition, then_body, else_body) => {
                let selected = if self.evaluate(condition, line, task)?.number(line)? != 0.0 {
                    then_body
                } else {
                    else_body
                };
                for nested in selected {
                    match self.execute_statement(nested, address, line, task, dispatcher)? {
                        Flow::Next => {}
                        flow => return Ok(flow),
                    }
                }
                Ok(Flow::Next)
            }
            Statement::IfBlock(condition) => {
                if self.evaluate(condition, line, task)?.number(line)? != 0.0 {
                    Ok(Flow::Next)
                } else {
                    let end = self
                        .if_blocks
                        .get(&address)
                        .copied()
                        .ok_or_else(|| program_error(line, "IF has no matching ENDIF"))?;
                    Ok(Flow::Jump(end + 1))
                }
            }
            Statement::EndIf => Ok(Flow::Next),
            Statement::Goto(target) => self.jump_to_line(*target, line),
            Statement::Gosub(target) => {
                let destination = self.line_address(*target, line)?;
                self.returns.push(ReturnFrame {
                    kind: ReturnKind::Subroutine,
                    address: address + 1,
                    saved_variables: Vec::new(),
                });
                Ok(Flow::Jump(destination))
            }
            Statement::Dim(declarations) => {
                for declaration in declarations {
                    self.dim(declaration, line, task)?;
                }
                Ok(Flow::Next)
            }
            Statement::Read(targets) => {
                for target in targets {
                    let Some((_, expression)) = self.data.get(self.data_cursor).cloned() else {
                        return Err(program_error(line, "READ passed the end of DATA"));
                    };
                    self.data_cursor += 1;
                    let value = self.evaluate(&expression, line, task)?;
                    self.assign(target, value, line, task)?;
                }
                Ok(Flow::Next)
            }
            Statement::Data(_) | Statement::NoOp | Statement::DefineProcedure(_, _) => {
                Ok(Flow::Next)
            }
            Statement::Restore(target) => {
                self.data_cursor = match target {
                    Some(line) => self.data.partition_point(|(data_line, _)| data_line < line),
                    None => 0,
                };
                Ok(Flow::Next)
            }
            Statement::For {
                variable,
                start,
                end,
                step,
            } => self.start_for(variable, start, end, step.as_ref(), address, line, task),
            Statement::Next(variable) => self.next_for(variable.as_deref(), address, line),
            Statement::Repeat => {
                self.repeat_loops.push(address + 1);
                Ok(Flow::Next)
            }
            Statement::Until(condition) => {
                let Some(repeat_address) = self.repeat_loops.last().copied() else {
                    return Err(program_error(line, "UNTIL has no matching REPEAT"));
                };
                if self.evaluate(condition, line, task)?.number(line)? != 0.0 {
                    self.repeat_loops.pop();
                    Ok(Flow::Next)
                } else {
                    Ok(Flow::Jump(repeat_address))
                }
            }
            Statement::ProcedureCall(name, arguments) => {
                #[cfg(feature = "experimental-jit")]
                if name.eq_ignore_ascii_case("IT") {
                    let values = self.evaluate_arguments(arguments, line, task)?;
                    if values.len() == 3 {
                        let real_c = values[0].number(line)?;
                        let imag_c = values[1].number(line)?;
                        let iteration_limit = values[2].number(line)?.trunc() as i32;
                        if let Some((iterations, [real_z, imag_z, u, v])) = self
                            .jit
                            .as_mut()
                            .and_then(|jit| jit.run_mandelbrot(real_c, imag_c, iteration_limit))
                        {
                            self.set_variable("IT%", Value::Number(f64::from(iterations)), line)?;
                            self.set_variable("E", Value::Number(real_z), line)?;
                            self.set_variable("F", Value::Number(imag_z), line)?;
                            self.set_variable("U", Value::Number(u), line)?;
                            self.set_variable("V", Value::Number(v), line)?;
                            return Ok(Flow::Next);
                        }
                    }
                    return self.execute_procedure_call_values(name, values, address, line);
                }

                let Some(definition) = self.program.procedures.get(name).cloned() else {
                    return Err(program_error(
                        line,
                        format!("unknown procedure PROC {name}"),
                    ));
                };
                let values = self.evaluate_arguments(arguments, line, task)?;
                self.enter_procedure(name, definition, values, address, line)
            }
            Statement::DefineFunction(_, _) => Ok(Flow::Next),
            Statement::FunctionReturn(_) => Err(program_error(
                line,
                "function return was reached outside a function call",
            )),
            Statement::Return => self.return_from(ReturnKind::Subroutine, line),
            Statement::EndProcedure => self.return_from(ReturnKind::Procedure, line),
            Statement::End => Ok(Flow::Stop),
            Statement::Sys {
                name,
                arguments,
                results,
            } => {
                let swi_name = String::from_utf8_lossy(name).to_ascii_uppercase();
                let mut context = SwiContext::default();
                for (register, argument) in arguments.iter().enumerate().take(10) {
                    if let Some(argument) = argument {
                        context.registers[register] =
                            self.evaluate(argument, line, task)?.number(line)?.trunc() as i32
                                as u32;
                    }
                }
                dispatcher.dispatch_named_swi(&swi_name, &mut context)?;
                for (register, target) in results.iter().enumerate() {
                    self.set_variable(
                        target,
                        Value::Number(f64::from(context.registers[register] as i32)),
                        line,
                    )?;
                }
                Ok(Flow::Next)
            }
            Statement::Call(_) => Err(program_error(
                line,
                "machine-code CALL requires a matching processor compatibility service not available in the hosted profile",
            )),
            Statement::StarCommand(command) => {
                self.execute_star_command(command, line)?;
                Ok(Flow::Next)
            }
        }
    }

    fn jump_to_line(&self, target: u16, line: u16) -> Result<Flow, RuntimeError> {
        Ok(Flow::Jump(self.line_address(target, line)?))
    }

    fn line_address(&self, target: u16, line: u16) -> Result<usize, RuntimeError> {
        self.program
            .line_entries
            .get(&target)
            .copied()
            .ok_or_else(|| program_error(line, format!("line {target} does not exist")))
    }

    #[cfg(feature = "experimental-jit")]
    fn execute_procedure_call_values(
        &mut self,
        name: &str,
        values: Vec<Value>,
        address: usize,
        line: u16,
    ) -> Result<Flow, RuntimeError> {
        let Some(definition) = self.program.procedures.get(name).cloned() else {
            return Err(program_error(
                line,
                format!("unknown procedure PROC {name}"),
            ));
        };
        self.enter_procedure(name, definition, values, address, line)
    }

    fn enter_procedure(
        &mut self,
        name: &str,
        definition: super::parser::Definition,
        values: Vec<Value>,
        address: usize,
        line: u16,
    ) -> Result<Flow, RuntimeError> {
        if values.len() != definition.parameters.len() {
            return Err(program_error(
                line,
                format!("PROC {name} argument count mismatch"),
            ));
        }
        let saved_variables = self.bind_parameters(&definition.parameters, values, line)?;
        self.returns.push(ReturnFrame {
            kind: ReturnKind::Procedure,
            address: address + 1,
            saved_variables,
        });
        Ok(Flow::Jump(definition.entry))
    }

    fn return_from(&mut self, expected: ReturnKind, line: u16) -> Result<Flow, RuntimeError> {
        let Some(frame) = self.returns.last().cloned() else {
            return Err(program_error(line, "RETURN or ENDPROC has no active call"));
        };
        let matches = matches!(
            (expected, frame.kind),
            (ReturnKind::Procedure, ReturnKind::Procedure)
                | (ReturnKind::Subroutine, ReturnKind::Subroutine)
        );
        if !matches {
            return Err(program_error(line, "mismatched RETURN and ENDPROC"));
        }
        self.returns.pop();
        self.restore_variables(frame.saved_variables);
        Ok(Flow::Jump(frame.address))
    }

    fn evaluate_arguments(
        &mut self,
        arguments: &[Expr],
        line: u16,
        task: &Task,
    ) -> Result<Vec<Value>, RuntimeError> {
        arguments
            .iter()
            .map(|argument| self.evaluate(argument, line, task))
            .collect()
    }

    fn bind_parameters(
        &mut self,
        parameters: &[String],
        values: Vec<Value>,
        line: u16,
    ) -> Result<Vec<(String, Option<Value>)>, RuntimeError> {
        if parameters.len() != values.len() {
            return Err(program_error(
                line,
                "procedure or function argument count mismatch",
            ));
        }
        let saved = parameters
            .iter()
            .map(|name| (name.clone(), self.variables.get(name).cloned()))
            .collect::<Vec<_>>();
        for (name, value) in parameters.iter().zip(values) {
            if let Err(error) = self.set_variable(name, value, line) {
                self.restore_variables(saved);
                return Err(error);
            }
        }
        Ok(saved)
    }

    fn restore_variables(&mut self, saved: Vec<(String, Option<Value>)>) {
        for (name, value) in saved.into_iter().rev() {
            if let Some(value) = value {
                self.variables.insert(name, value);
            } else {
                self.variables.remove(&name);
            }
        }
    }

    fn function_expression(
        &self,
        definition: &super::parser::Definition,
        name: &str,
        line: u16,
    ) -> Result<Expr, RuntimeError> {
        for instruction in self.program.instructions.iter().skip(definition.entry) {
            match &instruction.statement {
                Statement::FunctionReturn(expression) => return Ok(expression.clone()),
                Statement::DefineFunction(_, _) | Statement::DefineProcedure(_, _) => break,
                _ => {}
            }
        }
        Err(program_error(
            line,
            format!("FN {name} has no expression body"),
        ))
    }

    fn start_for(
        &mut self,
        variable: &str,
        start: &Expr,
        end: &Expr,
        step: Option<&Expr>,
        address: usize,
        line: u16,
        task: &mut Task,
    ) -> Result<Flow, RuntimeError> {
        let start_value = self.evaluate(start, line, task)?.number(line)?;
        let end_value = self.evaluate(end, line, task)?.number(line)?;
        let step_value = match step {
            Some(expression) => self.evaluate(expression, line, task)?.number(line)?,
            None => 1.0,
        };
        if step_value == 0.0 {
            return Err(program_error(line, "FOR STEP cannot be zero"));
        }
        let next_address = self
            .for_pairs
            .get(&address)
            .copied()
            .ok_or_else(|| program_error(line, "FOR has no matching NEXT"))?;
        self.set_variable(variable, Value::Number(start_value), line)?;

        let enters_loop = if step_value > 0.0 {
            start_value <= end_value
        } else {
            start_value >= end_value
        };
        if !enters_loop {
            return Ok(Flow::Jump(next_address + 1));
        }

        self.for_loops.push(ForFrame {
            variable: variable.to_owned(),
            limit: end_value,
            step: step_value,
            body_address: address + 1,
            next_address,
        });
        Ok(Flow::Next)
    }

    fn next_for(
        &mut self,
        requested_variable: Option<&str>,
        address: usize,
        line: u16,
    ) -> Result<Flow, RuntimeError> {
        let Some(frame) = self.for_loops.last() else {
            return Err(program_error(line, "NEXT has no active FOR"));
        };
        if frame.next_address != address {
            return Err(program_error(line, "NEXT does not match the active FOR"));
        }
        if requested_variable.is_some_and(|name| name != frame.variable) {
            return Err(program_error(line, "NEXT variable does not match FOR"));
        }

        let variable = frame.variable.clone();
        let step = frame.step;
        let limit = frame.limit;
        let body_address = frame.body_address;
        let current = self.get_variable(&variable).number(line)?;
        let next_value = current + step;
        self.set_variable(&variable, Value::Number(next_value), line)?;
        let continues = if step > 0.0 {
            next_value <= limit
        } else {
            next_value >= limit
        };
        if continues {
            Ok(Flow::Jump(body_address))
        } else {
            self.for_loops.pop();
            Ok(Flow::Next)
        }
    }

    fn dim(
        &mut self,
        declaration: &DimDeclaration,
        line: u16,
        task: &mut Task,
    ) -> Result<(), RuntimeError> {
        if declaration.dimensions.is_empty() {
            self.variables
                .insert(declaration.name.clone(), default_value(&declaration.name));
            return Ok(());
        }
        let mut length = 1_usize;
        for dimension in &declaration.dimensions {
            let upper_bound = self.evaluate(dimension, line, task)?.number(line)?;
            if upper_bound < 0.0 || upper_bound > 1_000_000.0 {
                return Err(program_error(
                    line,
                    "DIM size is outside the supported range",
                ));
            }
            let axis = upper_bound.trunc() as usize + 1;
            length = length
                .checked_mul(axis)
                .ok_or_else(|| program_error(line, "DIM array size overflowed"))?;
        }

        if declaration.byte_block {
            let byte_count =
                u32::try_from(length).map_err(|_| program_error(line, "DIM block is too large"))?;
            let block_start = self
                .next_heap_address
                .checked_add(3)
                .map(|address| address & !3)
                .ok_or_else(|| program_error(line, "DIM block address overflowed"))?;
            let block_end = block_start
                .checked_add(byte_count)
                .ok_or_else(|| program_error(line, "DIM block address overflowed"))?;
            let memory_end = GUEST_MEMORY_BASE
                .checked_add(u32::try_from(GUEST_MEMORY_SIZE).unwrap_or(u32::MAX))
                .ok_or_else(|| program_error(line, "task memory size overflowed"))?;
            if block_end > memory_end {
                return Err(program_error(line, "DIM block exceeds task memory"));
            }
            task.memory
                .write_bytes(self.next_heap_address, &vec![0; length])?;
            self.set_variable(
                &declaration.name,
                Value::Number(f64::from(block_start)),
                line,
            )?;
            self.next_heap_address = block_end;
        } else {
            self.arrays.insert(
                declaration.name.clone(),
                vec![default_value(&declaration.name); length],
            );
        }
        Ok(())
    }

    fn evaluate(
        &mut self,
        expression: &Expr,
        line: u16,
        task: &Task,
    ) -> Result<Value, RuntimeError> {
        match expression {
            Expr::Number(value) => Ok(Value::Number(*value)),
            Expr::String(value) => Ok(Value::String(value.clone())),
            Expr::Variable(name) if name == "TIME" => {
                let ticks = self.started.elapsed().as_millis() / 10;
                Ok(Value::Number(ticks as f64))
            }
            Expr::Variable(name) => Ok(self.get_variable(name)),
            Expr::ArrayElement(name, index) => {
                let index = self.evaluate(index, line, task)?.number(line)?;
                let index = array_index(index, line)?;
                let Some(array) = self.arrays.get(name) else {
                    return Err(program_error(
                        line,
                        format!("array {name} was not dimensioned"),
                    ));
                };
                array.get(index).cloned().ok_or_else(|| {
                    program_error(line, format!("array index {index} is out of range"))
                })
            }
            Expr::Unary(operator, operand) => {
                let operand = self.evaluate(operand, line, task)?.number(line)?;
                let value = match operator {
                    UnaryOp::Plus => operand,
                    UnaryOp::Minus => -operand,
                    UnaryOp::Not => f64::from(!(operand as i32)),
                };
                Ok(Value::Number(value))
            }
            Expr::Binary(left, operator, right) => {
                let left = self.evaluate(left, line, task)?;
                let right = self.evaluate(right, line, task)?;
                self.evaluate_binary(left, *operator, right, line)
            }
            Expr::Builtin(token, arguments) => self.evaluate_builtin(*token, arguments, line, task),
            Expr::UserFunction(name, arguments) => {
                let Some(definition) = self.program.functions.get(name).cloned() else {
                    return Err(program_error(line, format!("unknown function FN {name}")));
                };
                let values = self.evaluate_arguments(arguments, line, task)?;
                if values.len() != definition.parameters.len() {
                    return Err(program_error(
                        line,
                        format!("FN {name} argument count mismatch"),
                    ));
                }
                let expression = self.function_expression(&definition, name, line)?;
                let saved = self.bind_parameters(&definition.parameters, values, line)?;
                let result = self.evaluate(&expression, line, task);
                self.restore_variables(saved.clone());
                result
            }
            Expr::MemoryRead(width, address) => {
                let address = self.evaluate(address, line, task)?.number(line)?;
                self.read_memory(*width, address, line, task)
            }
        }
    }

    fn next_random_u32(&mut self) -> u32 {
        let mut value = self.random_state;
        value ^= value.wrapping_shl(13);
        value ^= value.wrapping_shr(17);
        value ^= value.wrapping_shl(5);
        self.random_state = value;
        value
    }

    fn evaluate_binary(
        &self,
        left: Value,
        operator: BinaryOp,
        right: Value,
        line: u16,
    ) -> Result<Value, RuntimeError> {
        if operator == BinaryOp::Add {
            if let (Value::String(left), Value::String(right)) = (&left, &right) {
                let mut joined = left.clone();
                joined.extend_from_slice(right);
                return Ok(Value::String(joined));
            }
        }
        if matches!(
            operator,
            BinaryOp::Equal
                | BinaryOp::NotEqual
                | BinaryOp::Less
                | BinaryOp::LessEqual
                | BinaryOp::Greater
                | BinaryOp::GreaterEqual
        ) {
            let ordering = match (&left, &right) {
                (Value::String(left), Value::String(right)) => left.cmp(right),
                (Value::Number(left), Value::Number(right)) => {
                    left.partial_cmp(right).unwrap_or(std::cmp::Ordering::Equal)
                }
                _ => return Err(program_error(line, "cannot compare a number with a string")),
            };
            let result = match operator {
                BinaryOp::Equal => ordering.is_eq(),
                BinaryOp::NotEqual => !ordering.is_eq(),
                BinaryOp::Less => ordering.is_lt(),
                BinaryOp::LessEqual => !ordering.is_gt(),
                BinaryOp::Greater => ordering.is_gt(),
                BinaryOp::GreaterEqual => !ordering.is_lt(),
                _ => unreachable!(),
            };
            return Ok(Value::Number(if result { 1.0 } else { 0.0 }));
        }

        let left = left.number(line)?;
        let right = right.number(line)?;
        let value = match operator {
            BinaryOp::Add => left + right,
            BinaryOp::Subtract => left - right,
            BinaryOp::Multiply => left * right,
            BinaryOp::Divide => {
                if right == 0.0 {
                    return Err(program_error(line, "division by zero"));
                }
                left / right
            }
            BinaryOp::IntegerDivide => {
                let divisor = right as i32;
                if divisor == 0 {
                    return Err(program_error(line, "integer division by zero"));
                }
                f64::from(
                    (left as i32)
                        .checked_div(divisor)
                        .ok_or_else(|| program_error(line, "integer division overflowed"))?,
                )
            }
            BinaryOp::Modulo => {
                let divisor = right as i32;
                if divisor == 0 {
                    return Err(program_error(line, "MOD by zero"));
                }
                f64::from(
                    (left as i32)
                        .checked_rem(divisor)
                        .ok_or_else(|| program_error(line, "MOD overflowed"))?,
                )
            }
            BinaryOp::Power => left.powf(right),
            BinaryOp::And => f64::from((left as i32) & (right as i32)),
            BinaryOp::Or => f64::from((left as i32) | (right as i32)),
            BinaryOp::ShiftLeft => f64::from((left as i32).wrapping_shl((right as u32) & 31)),
            BinaryOp::Equal
            | BinaryOp::NotEqual
            | BinaryOp::Less
            | BinaryOp::LessEqual
            | BinaryOp::Greater
            | BinaryOp::GreaterEqual => unreachable!(),
        };
        Ok(Value::Number(value))
    }

    fn evaluate_builtin(
        &mut self,
        token: u8,
        arguments: &[Expr],
        line: u16,
        task: &Task,
    ) -> Result<Value, RuntimeError> {
        let mut values = Vec::with_capacity(arguments.len());
        for argument in arguments {
            values.push(self.evaluate(argument, line, task)?);
        }
        let one_number = || -> Result<f64, RuntimeError> {
            if values.len() != 1 {
                return Err(program_error(
                    line,
                    "built-in function expects one argument",
                ));
            }
            values[0].number(line)
        };

        match token {
            0x97 => {
                if values.len() != 1 {
                    return Err(program_error(line, "ASC expects one argument"));
                }
                Ok(Value::Number(
                    values[0]
                        .string(line)?
                        .first()
                        .copied()
                        .map_or(-1.0, f64::from),
                ))
            }
            0x94 => Ok(Value::Number(one_number()?.abs())),
            0x9B => Ok(Value::Number(one_number()?.cos())),
            0xB5 => Ok(Value::Number(one_number()?.sin())),
            0xA8 => Ok(Value::Number(one_number()?.floor())),
            0xA9 => {
                if values.len() != 1 {
                    return Err(program_error(line, "LEN expects one argument"));
                }
                Ok(Value::Number(values[0].string(line)?.len() as f64))
            }
            0xAA => Ok(Value::Number(one_number()?.ln())),
            0xAB => Ok(Value::Number(one_number()?.log10())),
            0xA6 => {
                if values.len() > 1 {
                    return Err(program_error(line, "INKEY expects zero or one argument"));
                }
                let no_key = if values.is_empty() { -256.0 } else { -1.0 };
                Ok(Value::Number(
                    self.pending_key.take().map_or(no_key, f64::from),
                ))
            }
            0xB3 => {
                if values.len() > 1 {
                    return Err(program_error(line, "RND expects zero or one argument"));
                }
                let value = if values.is_empty() {
                    f64::from(self.next_random_u32() as i32)
                } else {
                    let argument = values[0].number(line)?;
                    let integer = argument.trunc() as i64;
                    if argument < 0.0 {
                        let seed = integer as i32;
                        self.random_state = if seed == 0 { 0xA341_316C } else { seed as u32 };
                        f64::from(seed)
                    } else {
                        match integer {
                            0 => self.last_random_fraction,
                            1 => {
                                self.last_random_fraction =
                                    f64::from(self.next_random_u32()) / 4_294_967_296.0;
                                self.last_random_fraction
                            }
                            upper => {
                                let upper = u32::try_from(upper)
                                    .map_err(|_| program_error(line, "RND limit is too large"))?;
                                f64::from(self.next_random_u32() % upper + 1)
                            }
                        }
                    }
                };
                Ok(Value::Number(value))
            }
            0xA7 => {
                if !(2..=3).contains(&values.len()) {
                    return Err(program_error(line, "INSTR expects two or three arguments"));
                }
                let text = values[0].string(line)?;
                let pattern = values[1].string(line)?;
                let start = if values.len() == 3 {
                    bounded_string_length(values[2].number(line)?, line)?.saturating_sub(1)
                } else {
                    0
                };
                let position = if start > text.len() {
                    None
                } else {
                    text[start..]
                        .windows(pattern.len().max(1))
                        .position(|window| !pattern.is_empty() && window == pattern)
                        .map(|offset| start + offset + 1)
                };
                Ok(Value::Number(position.map_or(0.0, |index| index as f64)))
            }
            0xB6 => Ok(Value::Number(one_number()?.sqrt())),
            0xB7 => Ok(Value::Number(one_number()?.tan())),
            0xBC => {
                if values.len() != 1 {
                    return Err(program_error(line, "VAL expects one argument"));
                }
                Ok(Value::Number(parse_basic_val(values[0].string(line)?)))
            }
            0xBD => {
                let byte = one_number()?.trunc() as i32 as u8;
                Ok(Value::String(vec![byte]))
            }
            0xC0 => {
                require_argument_count(&values, 2, line, "LEFT$")?;
                let text = values[0].string(line)?;
                let count = bounded_string_length(values[1].number(line)?, line)?;
                Ok(Value::String(text[..text.len().min(count)].to_vec()))
            }
            0xC1 => {
                if !(2..=3).contains(&values.len()) {
                    return Err(program_error(line, "MID$ expects two or three arguments"));
                }
                let text = values[0].string(line)?;
                let start = values[1].number(line)?.trunc().max(1.0) as usize - 1;
                let length = if values.len() == 3 {
                    bounded_string_length(values[2].number(line)?, line)?
                } else {
                    text.len()
                };
                let end = start.saturating_add(length).min(text.len());
                let start = start.min(text.len());
                Ok(Value::String(text[start..end].to_vec()))
            }
            0xC2 => {
                require_argument_count(&values, 2, line, "RIGHT$")?;
                let text = values[0].string(line)?;
                let count = bounded_string_length(values[1].number(line)?, line)?;
                Ok(Value::String(
                    text[text.len().saturating_sub(count)..].to_vec(),
                ))
            }
            0xC3 => Ok(Value::String(format_number(one_number()?).into_bytes())),
            0xC4 => {
                require_argument_count(&values, 2, line, "STRING$")?;
                let count = bounded_string_length(values[0].number(line)?, line)?;
                let pattern = values[1].string(line)?;
                let byte = pattern.first().copied().unwrap_or_default();
                Ok(Value::String(vec![byte; count]))
            }
            _ => Err(program_error(
                line,
                format!("unsupported built-in token &{token:02X}"),
            )),
        }
    }

    fn get_variable(&self, name: &str) -> Value {
        self.variables
            .get(name)
            .cloned()
            .unwrap_or_else(|| default_value(name))
    }

    #[cfg(feature = "experimental-jit")]
    fn integer_variable(&self, name: &str, line: u16) -> Result<i32, RuntimeError> {
        Ok(self.get_variable(name).number(line)?.trunc() as i32)
    }

    fn set_variable(&mut self, name: &str, value: Value, line: u16) -> Result<(), RuntimeError> {
        let value = if name.ends_with('$') {
            match value {
                Value::String(_) => value,
                Value::Number(_) => {
                    return Err(program_error(
                        line,
                        format!("{name} requires a string value"),
                    ));
                }
            }
        } else {
            let number = value.number(line)?;
            Value::Number(if name.ends_with('%') {
                f64::from(number.trunc() as i32)
            } else {
                number
            })
        };
        self.variables.insert(name.to_owned(), value);
        Ok(())
    }

    fn assign(
        &mut self,
        target: &LValue,
        value: Value,
        line: u16,
        task: &mut Task,
    ) -> Result<(), RuntimeError> {
        match target {
            LValue::Variable(name) => self.set_variable(name, value, line),
            LValue::ArrayElement(name, index) => {
                let index = array_index(self.evaluate(index, line, task)?.number(line)?, line)?;
                let Some(array) = self.arrays.get_mut(name) else {
                    return Err(program_error(
                        line,
                        format!("array {name} was not dimensioned"),
                    ));
                };
                let Some(slot) = array.get_mut(index) else {
                    return Err(program_error(
                        line,
                        format!("array index {index} is out of range"),
                    ));
                };
                *slot = coerce_for_variable(name, value, line)?;
                Ok(())
            }
            LValue::Memory(width, address) => {
                let address = self.address_value(address, line, task)?;
                self.write_memory(*width, address, value, line, task)
            }
            LValue::MemoryByteAt(base, offset) => {
                let base = self.evaluate(base, line, task)?.number(line)?;
                let offset = self.evaluate(offset, line, task)?.number(line)?;
                let address = checked_guest_address(base + offset, line)?;
                self.write_memory(MemoryWidth::Byte, address, value, line, task)
            }
            LValue::MemoryOffset(width, base, offset) => {
                let base = self.evaluate(base, line, task)?.number(line)?;
                let offset = self.evaluate(offset, line, task)?.number(line)?;
                let address = checked_guest_address(base + offset, line)?;
                self.write_memory(*width, address, value, line, task)
            }
            LValue::MemoryString(address) => {
                let address = self.address_value(address, line, task)?;
                let bytes = value.string(line)?;
                if bytes.len() > 4096 {
                    return Err(program_error(line, "indirect string exceeds 4096 bytes"));
                }
                task.memory.write_bytes(address, bytes)?;
                let terminator =
                    address
                        .checked_add(u32::try_from(bytes.len()).map_err(|_| {
                            program_error(line, "indirect string address overflowed")
                        })?)
                        .ok_or_else(|| program_error(line, "indirect string address overflowed"))?;
                task.memory.write_byte(terminator, b'\r')?;
                Ok(())
            }
            LValue::StringSlice(name, start, length) => {
                let start = self.evaluate(start, line, task)?.number(line)?;
                let length =
                    bounded_string_length(self.evaluate(length, line, task)?.number(line)?, line)?;
                let replacement = value.string(line)?.to_vec();
                let start = start.trunc().max(1.0) as usize - 1;
                let Some(Value::String(target)) = self.variables.get_mut(name) else {
                    return Err(program_error(
                        line,
                        format!("{name} is not a string variable"),
                    ));
                };
                let start = start.min(target.len());
                let end = start.saturating_add(length).min(target.len());
                let replacement = &replacement[..replacement.len().min(length)];
                target.splice(start..end, replacement.iter().copied());
                Ok(())
            }
        }
    }

    fn address_value(
        &mut self,
        expression: &Expr,
        line: u16,
        task: &Task,
    ) -> Result<u32, RuntimeError> {
        checked_guest_address(self.evaluate(expression, line, task)?.number(line)?, line)
    }

    fn read_memory(
        &self,
        width: MemoryWidth,
        address: f64,
        line: u16,
        task: &Task,
    ) -> Result<Value, RuntimeError> {
        let address = checked_guest_address(address, line)?;
        let value =
            match width {
                MemoryWidth::Byte => u32::from(task.memory.read_byte(address)?),
                MemoryWidth::Word => {
                    let bytes =
                        [
                            task.memory.read_byte(address)?,
                            task.memory
                                .read_byte(address.checked_add(1).ok_or_else(|| {
                                    program_error(line, "memory address overflowed")
                                })?)?,
                            task.memory
                                .read_byte(address.checked_add(2).ok_or_else(|| {
                                    program_error(line, "memory address overflowed")
                                })?)?,
                            task.memory
                                .read_byte(address.checked_add(3).ok_or_else(|| {
                                    program_error(line, "memory address overflowed")
                                })?)?,
                        ];
                    u32::from_le_bytes(bytes) as i32 as u32
                }
            };
        let signed = match width {
            MemoryWidth::Byte => value as i32,
            MemoryWidth::Word => value as i32,
        };
        Ok(Value::Number(f64::from(signed)))
    }

    fn write_memory(
        &self,
        width: MemoryWidth,
        address: u32,
        value: Value,
        line: u16,
        task: &mut Task,
    ) -> Result<(), RuntimeError> {
        let number = value.number(line)? as i32;
        match width {
            MemoryWidth::Byte => task.memory.write_byte(address, number as u8)?,
            MemoryWidth::Word => task.memory.write_bytes(address, &number.to_le_bytes())?,
        }
        Ok(())
    }

    fn read_input(
        &mut self,
        line: u16,
        task: &mut Task,
        dispatcher: &mut SwiDispatcher,
    ) -> Result<Value, RuntimeError> {
        dispatcher.write_inline(task, b"? ")?;
        let mut input = SwiContext::default();
        input.registers[0] = INPUT_BUFFER;
        input.registers[1] = INPUT_BUFFER_SIZE - 1;
        input.registers[2] = u32::from(b' ');
        input.registers[3] = u32::from(u8::MAX);
        dispatcher.dispatch(OS_READ_LINE, task, &mut input)?;
        if input.carry {
            return Err(program_error(line, "input interrupted"));
        }

        let length = input.registers[1];
        if length >= INPUT_BUFFER_SIZE {
            return Err(program_error(
                line,
                "OS_ReadLine returned an invalid string length",
            ));
        }
        let terminator = INPUT_BUFFER
            .checked_add(length)
            .ok_or(crate::memory::MemoryError::AddressOverflow)?;
        task.memory.write_byte(terminator, 0)?;
        let value = task
            .memory
            .read_c_string(INPUT_BUFFER, INPUT_BUFFER_SIZE as usize)?;
        Ok(Value::String(value))
    }

    fn print(
        &mut self,
        items: &[PrintItem],
        line: u16,
        task: &mut Task,
        dispatcher: &mut SwiDispatcher,
    ) -> Result<(), RuntimeError> {
        for item in items {
            match item {
                PrintItem::Value(expression) => {
                    let value = self.evaluate(expression, line, task)?;
                    let bytes = match value {
                        Value::String(value) => value,
                        Value::Number(value) => {
                            format_print_number(value, self.print_format).into_bytes()
                        }
                    };
                    self.emit(&bytes, task, dispatcher)?;
                }
                PrintItem::Spaces(expression) => {
                    let count = self.evaluate(expression, line, task)?.number(line)?;
                    let count = bounded_string_length(count, line)?;
                    self.emit(&vec![b' '; count], task, dispatcher)?;
                }
                PrintItem::Tab(x, y) => {
                    let x = self.evaluate(x, line, task)?.number(line)?.trunc() as u8;
                    let y = self.evaluate(y, line, task)?.number(line)?.trunc() as u8;
                    dispatcher.write_via_os_write_c(task, &[31, x, y])?;
                    self.print_column = usize::from(x);
                }
                PrintItem::Comma => {
                    let remainder = self.print_column % PRINT_ZONE_WIDTH;
                    let count = PRINT_ZONE_WIDTH - remainder;
                    self.emit(&vec![b' '; count], task, dispatcher)?;
                }
                PrintItem::Semicolon => {}
                PrintItem::NewLine => self.new_line(task, dispatcher)?,
            }
        }

        let suppress_final_newline =
            matches!(items.last(), Some(PrintItem::Semicolon | PrintItem::Comma));
        if !suppress_final_newline {
            self.new_line(task, dispatcher)?;
        }
        Ok(())
    }

    fn emit(
        &mut self,
        bytes: &[u8],
        task: &mut Task,
        dispatcher: &mut SwiDispatcher,
    ) -> Result<(), RuntimeError> {
        if bytes.is_empty() {
            return Ok(());
        }
        dispatcher.write_indirect(task, bytes)?;
        for byte in bytes {
            if *byte == b'\n' || *byte == b'\r' {
                self.print_column = 0;
            } else {
                self.print_column = self.print_column.saturating_add(1);
            }
        }
        Ok(())
    }

    fn new_line(
        &mut self,
        task: &mut Task,
        dispatcher: &mut SwiDispatcher,
    ) -> Result<(), RuntimeError> {
        dispatcher.dispatch(OS_NEW_LINE, task, &mut SwiContext::default())?;
        self.print_column = 0;
        Ok(())
    }

    fn execute_star_command(&self, command: &[u8], line: u16) -> Result<(), RuntimeError> {
        let command = String::from_utf8_lossy(command);
        let normalized: String = command
            .chars()
            .filter(|character| !character.is_ascii_whitespace())
            .flat_map(char::to_uppercase)
            .collect();
        if normalized == "FX151,78,243" {
            // ClockSP5 resets machine-specific display/timing state here.
            // The hosted profile has no such hardware state to restore.
            return Ok(());
        }
        Err(program_error(
            line,
            format!("MOS command *{command} is not available in the hosted profile"),
        ))
    }
}

fn match_for_loops(program: &ParsedProgram) -> HashMap<usize, usize> {
    let mut stack = Vec::new();
    let mut matches = HashMap::new();
    for (address, instruction) in program.instructions.iter().enumerate() {
        match &instruction.statement {
            Statement::For { .. } => stack.push(address),
            Statement::Next(_) => {
                if let Some(start) = stack.pop() {
                    matches.insert(start, address);
                }
            }
            _ => {}
        }
    }
    matches
}

fn match_if_blocks(program: &ParsedProgram) -> HashMap<usize, usize> {
    let mut stack = Vec::new();
    let mut matches = HashMap::new();
    for (address, instruction) in program.instructions.iter().enumerate() {
        match &instruction.statement {
            Statement::IfBlock(_) => stack.push(address),
            Statement::EndIf => {
                if let Some(start) = stack.pop() {
                    matches.insert(start, address);
                }
            }
            _ => {}
        }
    }
    matches
}

fn default_value(name: &str) -> Value {
    if name.ends_with('$') {
        Value::String(Vec::new())
    } else {
        Value::Number(0.0)
    }
}

fn coerce_for_variable(name: &str, value: Value, line: u16) -> Result<Value, RuntimeError> {
    if name.ends_with('$') {
        match value {
            string @ Value::String(_) => Ok(string),
            Value::Number(_) => Err(program_error(
                line,
                format!("{name} requires a string value"),
            )),
        }
    } else {
        let number = value.number(line)?;
        Ok(Value::Number(if name.ends_with('%') {
            f64::from(number.trunc() as i32)
        } else {
            number
        }))
    }
}

fn array_index(number: f64, line: u16) -> Result<usize, RuntimeError> {
    if !number.is_finite() || number < 0.0 || number > usize::MAX as f64 {
        return Err(program_error(
            line,
            "array index is outside the supported range",
        ));
    }
    Ok(number.trunc() as usize)
}

fn checked_guest_address(number: f64, line: u16) -> Result<u32, RuntimeError> {
    if !number.is_finite() || number < 0.0 || number > f64::from(u32::MAX) {
        return Err(program_error(
            line,
            "memory address is outside the logical address range",
        ));
    }
    Ok(number.trunc() as u32)
}

fn bounded_string_length(number: f64, line: u16) -> Result<usize, RuntimeError> {
    if !number.is_finite() || number < 0.0 || number > 1_000_000.0 {
        return Err(program_error(
            line,
            "string length is outside the supported range",
        ));
    }
    Ok(number.trunc() as usize)
}

fn parse_basic_val(bytes: &[u8]) -> f64 {
    let text = String::from_utf8_lossy(bytes);
    let text = text.trim_start();
    let mut end = 0;
    let mut digits = 0;
    let mut exponent = false;
    let mut decimal = false;
    for (index, byte) in text.bytes().enumerate() {
        match byte {
            b'+' | b'-'
                if index == 0
                    || (exponent
                        && text
                            .as_bytes()
                            .get(index.wrapping_sub(1))
                            .is_some_and(|previous| matches!(previous, b'E' | b'e'))) =>
            {
                end = index + 1;
            }
            b'0'..=b'9' => {
                digits += 1;
                end = index + 1;
            }
            b'.' if !decimal && !exponent => {
                decimal = true;
                end = index + 1;
            }
            b'E' | b'e' if !exponent && digits > 0 => {
                exponent = true;
                end = index + 1;
            }
            _ => break,
        }
    }
    if digits == 0 {
        0.0
    } else {
        text[..end].parse::<f64>().unwrap_or(0.0)
    }
}

fn graphics_coordinate(number: f64, line: u16) -> Result<i32, RuntimeError> {
    let coordinate = number.trunc();
    if !(i16::MIN as f64..=i16::MAX as f64).contains(&coordinate) {
        return Err(program_error(
            line,
            "graphics coordinate is outside the signed 16-bit range",
        ));
    }
    Ok(coordinate as i32)
}

fn format_number(number: f64) -> String {
    if number == 0.0 {
        return "0".into();
    }
    if number.is_finite() && number.fract() == 0.0 && number.abs() < 1.0e15 {
        return format!("{number:.0}");
    }
    number.to_string()
}

fn format_print_number(number: f64, format: u32) -> String {
    if !number.is_finite() {
        return format_number(number);
    }
    let style = (format >> 16) & 0xFF;
    let precision = ((format >> 8) & 0xFF).min(10) as usize;
    match style {
        2 => format!("{number:.precision$}"),
        1 => {
            let fractional_digits = precision.saturating_sub(1);
            format!("{number:.fractional_digits$e}").replace('e', "E")
        }
        _ => format_number(number),
    }
}

fn require_argument_count(
    values: &[Value],
    expected: usize,
    line: u16,
    function: &str,
) -> Result<(), RuntimeError> {
    if values.len() != expected {
        Err(program_error(
            line,
            format!("{function} expects {expected} arguments"),
        ))
    } else {
        Ok(())
    }
}

fn program_error(line: u16, message: impl AsRef<str>) -> RuntimeError {
    RuntimeError::Program(format!("line {line}: {}", message.as_ref()))
}
