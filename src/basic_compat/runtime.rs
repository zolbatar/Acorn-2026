use std::{
    collections::{HashMap, VecDeque},
    sync::{Arc, Mutex},
};

use crate::{
    error::RuntimeError,
    memory::{GUEST_MEMORY_BASE, GUEST_MEMORY_SIZE, Task},
    swi::{MosClock, OS_CLI, OS_NEW_LINE, OS_READ_LINE, SwiContext, SwiDispatcher},
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
pub(crate) enum Value {
    Number(f64),
    /// Exact native integer literal, normalized when assigned to a typed
    /// System Profile binding.
    Integer(i128),
    UInt64(u64),
    Int64(i64),
    String(Vec<u8>),
    Enum {
        type_name: String,
        value: i64,
    },
    Flags {
        type_name: String,
        value: u64,
    },
    Record {
        type_name: String,
        fields: HashMap<String, Value>,
        readonly_fields: std::collections::HashSet<String>,
    },
    Handle {
        type_name: String,
        raw: u32,
    },
    LogicalAddress {
        owner_task: u64,
        raw: u32,
    },
    Error {
        type_name: String,
        code: u32,
        message: Vec<u8>,
        fields: HashMap<String, Value>,
        readonly_fields: std::collections::HashSet<String>,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ExactIntegerKind {
    Literal,
    Signed64,
    Unsigned64,
}

fn exact_integer(value: &Value) -> Option<(i128, ExactIntegerKind)> {
    match value {
        Value::Integer(value) => Some((*value, ExactIntegerKind::Literal)),
        Value::Int64(value) => Some((i128::from(*value), ExactIntegerKind::Signed64)),
        Value::UInt64(value) => Some((i128::from(*value), ExactIntegerKind::Unsigned64)),
        _ => None,
    }
}

fn exact_integer_result(
    kind: ExactIntegerKind,
    value: i128,
    line: u16,
) -> Result<Value, RuntimeError> {
    match kind {
        ExactIntegerKind::Literal => Ok(Value::Integer(value)),
        ExactIntegerKind::Signed64 => i64::try_from(value)
            .map(Value::Int64)
            .map_err(|_| program_error(line, "INT64 arithmetic overflowed")),
        ExactIntegerKind::Unsigned64 => u64::try_from(value)
            .map(Value::UInt64)
            .map_err(|_| program_error(line, "UINT64 arithmetic overflowed")),
    }
}

fn exact_integer_pair(
    left: &Value,
    right: &Value,
    line: u16,
) -> Result<Option<(i128, i128, ExactIntegerKind)>, RuntimeError> {
    let (Some((left_value, left_kind)), Some((right_value, right_kind))) =
        (exact_integer(left), exact_integer(right))
    else {
        return Ok(None);
    };
    let kind = match (left_kind, right_kind) {
        (left, right) if left == right => left,
        (ExactIntegerKind::Literal, kind) | (kind, ExactIntegerKind::Literal) => kind,
        _ => {
            return Err(program_error(
                line,
                "signed and unsigned 64-bit operands require an explicit conversion",
            ));
        }
    };
    Ok(Some((left_value, right_value, kind)))
}

fn coerce_exact_literal_with_float(
    left: Value,
    right: Value,
    line: u16,
) -> Result<(Value, Value), RuntimeError> {
    match (left, right) {
        (Value::Number(number), Value::Integer(integer)) => Ok((
            Value::Number(number),
            Value::Number(exact_i128_to_f64(integer, line).map_err(|_| {
                program_error(
                    line,
                    "integer literal is not exactly representable with a floating operand; use an explicit conversion",
                )
            })?),
        )),
        (Value::Integer(integer), Value::Number(number)) => Ok((
            Value::Number(exact_i128_to_f64(integer, line).map_err(|_| {
                program_error(
                    line,
                    "integer literal is not exactly representable with a floating operand; use an explicit conversion",
                )
            })?),
            Value::Number(number),
        )),
        (left, right) => Ok((left, right)),
    }
}

#[derive(Clone, Debug, Default)]
pub(super) struct ModuleWorkspace(Arc<Mutex<HashMap<String, Value>>>);

#[derive(Clone, Debug)]
pub(super) struct ModuleWorkspaceSnapshot(HashMap<String, Value>);

impl ModuleWorkspace {
    pub(super) fn snapshot(&self) -> ModuleWorkspaceSnapshot {
        ModuleWorkspaceSnapshot(
            self.0
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone(),
        )
    }

    pub(super) fn restore(&self, snapshot: ModuleWorkspaceSnapshot) {
        *self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = snapshot.0;
    }

    #[cfg(test)]
    pub(super) fn read_number(&self, name: &str) -> Option<f64> {
        match self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(name)?
        {
            Value::Number(value) => Some(*value),
            Value::Integer(value) => Some(*value as f64),
            Value::UInt64(value) => Some(*value as f64),
            Value::Int64(value) => Some(*value as f64),
            Value::Enum { value, .. } => Some(*value as f64),
            Value::Flags { value, .. } => Some(*value as f64),
            _ => None,
        }
    }
}

impl Value {
    fn number(&self, line: u16) -> Result<f64, RuntimeError> {
        match self {
            Self::Number(value) => Ok(*value),
            Self::Integer(value) => exact_i128_to_f64(*value, line),
            Self::UInt64(value) => exact_i128_to_f64(i128::from(*value), line),
            Self::Int64(value) => exact_i128_to_f64(i128::from(*value), line),
            Self::String(_)
            | Self::Record { .. }
            | Self::Handle { .. }
            | Self::LogicalAddress { .. }
            | Self::Error { .. } => Err(program_error(line, "expected a numeric expression")),
            Self::Enum { .. } | Self::Flags { .. } => Err(program_error(
                line,
                "enum and flag values are not ordinary numbers",
            )),
        }
    }

    fn string(&self, line: u16) -> Result<&[u8], RuntimeError> {
        match self {
            Self::String(value) => Ok(value),
            Self::Number(_)
            | Self::Integer(_)
            | Self::UInt64(_)
            | Self::Int64(_)
            | Self::Enum { .. }
            | Self::Flags { .. }
            | Self::Record { .. }
            | Self::Handle { .. }
            | Self::LogicalAddress { .. }
            | Self::Error { .. } => Err(program_error(line, "expected a string expression")),
        }
    }

    fn record_field(&self, name: &str, line: u16) -> Result<Value, RuntimeError> {
        match self {
            Self::Record { fields, .. } | Self::Error { fields, .. } => fields
                .get(name)
                .cloned()
                .ok_or_else(|| program_error(line, format!("unknown field {name}"))),
            _ => Err(program_error(
                line,
                "member access requires a record or structured error",
            )),
        }
    }

    fn truthy(&self, line: u16) -> Result<bool, RuntimeError> {
        match self {
            Self::Integer(value) => Ok(*value != 0),
            Self::UInt64(value) => Ok(*value != 0),
            Self::Int64(value) => Ok(*value != 0),
            value => value.number(line).map(|value| value != 0.0),
        }
    }

    fn number_string(&self, line: u16) -> Result<Vec<u8>, RuntimeError> {
        match self {
            Self::Integer(value) => Ok(value.to_string().into_bytes()),
            Self::UInt64(value) => Ok(value.to_string().into_bytes()),
            Self::Int64(value) => Ok(value.to_string().into_bytes()),
            value => Ok(format_number(value.number(line)?).into_bytes()),
        }
    }
}

fn exact_i128_to_f64(value: i128, line: u16) -> Result<f64, RuntimeError> {
    let converted = value as f64;
    let upper_bound = -(i128::MIN as f64);
    if !converted.is_finite()
        || converted < i128::MIN as f64
        || converted >= upper_bound
        || converted as i128 != value
    {
        return Err(program_error(
            line,
            "64-bit integer cannot be represented exactly as a floating value; use an explicit conversion",
        ));
    }
    Ok(converted)
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
    routine_name: Option<String>,
    local_readonly_scope: bool,
    inline_continuation: VecDeque<InlineContinuation>,
}

#[derive(Clone)]
struct InlineContinuation {
    statement: Statement,
    address: usize,
    line: u16,
}

struct ForFrame {
    variable: String,
    limit: Value,
    step: Value,
    body_address: usize,
    next_address: usize,
}

#[derive(Clone)]
struct TryRegion {
    catch_address: usize,
    end_address: usize,
    error_name: String,
    error_type: String,
}

struct ActiveTry {
    catch_address: usize,
    end_address: usize,
    error_name: String,
    error_type: String,
    return_depth: usize,
    for_depth: usize,
    repeat_depth: usize,
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
    inline_continuations: VecDeque<InlineContinuation>,
    for_loops: Vec<ForFrame>,
    repeat_loops: Vec<usize>,
    for_pairs: HashMap<usize, usize>,
    if_blocks: HashMap<usize, usize>,
    try_regions: HashMap<usize, TryRegion>,
    try_frames: Vec<ActiveTry>,
    completed_try_ends: std::collections::HashSet<usize>,
    readonly_bindings: std::collections::HashSet<String>,
    readonly_initialized: std::collections::HashSet<String>,
    local_readonly_scopes: Vec<HashMap<String, Option<Value>>>,
    clock: MosClock,
    steps: u64,
    interpreted_statement_count: u64,
    interpreted_expression_count: u64,
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
        // SAFETY: the task pointer is installed alongside the dispatcher and
        // remains valid for this synchronous callback.
        let task = unsafe { &*context.task };
        if let Some(key) = dispatcher.poll_key(task) {
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
        let try_regions = match_try_regions(&program);
        let readonly_bindings = program.readonly_bindings.iter().cloned().collect();

        Self {
            program,
            variables: HashMap::new(),
            arrays: HashMap::new(),
            data,
            data_cursor: 0,
            returns: Vec::new(),
            inline_continuations: VecDeque::new(),
            for_loops: Vec::new(),
            repeat_loops: Vec::new(),
            for_pairs,
            if_blocks,
            try_regions,
            try_frames: Vec::new(),
            completed_try_ends: std::collections::HashSet::new(),
            readonly_bindings,
            readonly_initialized: std::collections::HashSet::new(),
            local_readonly_scopes: Vec::new(),
            clock: MosClock::default(),
            steps: 0,
            interpreted_statement_count: 0,
            interpreted_expression_count: 0,
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
                    Some(
                        "no BASIC regions were optimized; the entire program was interpreted"
                            .into(),
                    )
                } else {
                    Some("BASIC control flow and unmatched statements used the interpreter".into())
                }
            });
        report.interpreted_statement_count = self.interpreted_statement_count;
        report.interpreted_expression_count = self.interpreted_expression_count;
        report
    }

    pub(super) fn run(
        &mut self,
        task: &mut Task,
        dispatcher: &mut SwiDispatcher,
    ) -> Result<(), RuntimeError> {
        self.clock = dispatcher.system_clock();
        let mut address = 0_usize;
        while address < self.program.instructions.len() || !self.inline_continuations.is_empty() {
            let inline_instruction = self.inline_continuations.pop_front();
            self.steps += 1;
            if self.steps & 0x3FF == 0 && self.pending_key.is_none() {
                if let Some(key) = dispatcher.poll_key(task) {
                    self.pending_key = Some(key);
                }
            }
            if self.steps > MAX_EXECUTION_STEPS {
                let line = inline_instruction.as_ref().map_or_else(
                    || self.program.instructions[address].line_number,
                    |item| item.line,
                );
                return Err(program_error(line, "execution step limit reached"));
            }

            #[cfg(feature = "experimental-jit")]
            if inline_instruction.is_none()
                && self
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
            if inline_instruction.is_none()
                && self
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
            if inline_instruction.is_none()
                && self
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
            if inline_instruction.is_none()
                && let Statement::ProcedureCall(name, arguments) =
                    self.program.instructions[address].statement.clone()
            {
                if let Some(variable_names) = self
                    .jit
                    .as_ref()
                    .and_then(|jit| jit.native_procedure_variables(&name))
                {
                    let line = self.program.instructions[address].line_number;
                    let values = self.evaluate_arguments(&arguments, line, task, dispatcher)?;
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

                    match self
                        .execute_procedure_call_values(&name, values, task.id, address, line)?
                    {
                        Flow::Next => address += 1,
                        Flow::Jump(destination) => address = destination,
                        Flow::Stop => break,
                    }
                    continue;
                }
            }

            #[cfg(feature = "experimental-jit")]
            if inline_instruction.is_none()
                && let Some(variables) = self
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

            let executing_inline = inline_instruction.is_some();
            let instruction_address = inline_instruction
                .as_ref()
                .map_or(address, |item| item.address);
            let instruction = inline_instruction.map_or_else(
                || self.program.instructions[address].clone(),
                |item| super::parser::LocatedStatement {
                    line_number: item.line,
                    statement: item.statement,
                },
            );
            let return_depth = self.returns.len();
            let executed = self.execute_statement(
                &instruction.statement,
                instruction_address,
                instruction.line_number,
                task,
                dispatcher,
            );
            let flow = match executed {
                Ok(flow) => flow,
                Err(error) => {
                    if let Some(catch_address) =
                        self.handle_runtime_error(&error, instruction.line_number, task.id)?
                    {
                        address = catch_address;
                        continue;
                    }
                    return Err(error);
                }
            };
            match flow {
                Flow::Next => {
                    if !executing_inline {
                        address += 1;
                    }
                }
                Flow::Jump(destination) => {
                    if self.returns.len() > return_depth {
                        if let Some(frame) = self.returns.last_mut() {
                            frame
                                .inline_continuation
                                .append(&mut self.inline_continuations);
                        }
                    } else if self.returns.len() == return_depth {
                        self.inline_continuations.clear();
                    }
                    address = destination;
                }
                Flow::Stop => {
                    self.inline_continuations.clear();
                    break;
                }
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
        self.interpreted_statement_count = self.interpreted_statement_count.saturating_add(1);
        match statement {
            Statement::Assign(target, expression) => {
                let value = self.evaluate(expression, line, task, dispatcher)?;
                self.assign(target, value, line, task, dispatcher)?;
                Ok(Flow::Next)
            }
            Statement::Input(target) => {
                let value = self.read_input(line, task, dispatcher)?;
                self.assign(target, value, line, task, dispatcher)?;
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
                let foreground = self
                    .evaluate(&colours[0], line, task, dispatcher)?
                    .number(line)?;
                dispatcher.write_via_os_write_c(task, &[17, foreground.trunc() as u8])?;
                if let Some(background) = colours.get(1) {
                    let background = self
                        .evaluate(background, line, task, dispatcher)?
                        .number(line)?;
                    dispatcher
                        .write_via_os_write_c(task, &[17, (background.trunc() as u8) | 0x80])?;
                }
                Ok(Flow::Next)
            }
            Statement::PrintFormat(expression) => {
                let format = self
                    .evaluate(expression, line, task, dispatcher)?
                    .number(line)?
                    .trunc() as i64;
                self.print_format = if format == 0 {
                    DEFAULT_PRINT_FORMAT
                } else {
                    format as u32
                };
                Ok(Flow::Next)
            }
            Statement::Mode(expression) => {
                let mode = self
                    .evaluate(expression, line, task, dispatcher)?
                    .number(line)?
                    .trunc();
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
                        .evaluate(&argument.value, line, task, dispatcher)?
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
                let x1 = self.evaluate(x1, line, task, dispatcher)?.number(line)?;
                let y1 = self.evaluate(y1, line, task, dispatcher)?.number(line)?;
                let x2 = self.evaluate(x2, line, task, dispatcher)?.number(line)?;
                let y2 = self.evaluate(y2, line, task, dispatcher)?.number(line)?;
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
                let x = self.evaluate(x, line, task, dispatcher)?.number(line)?;
                let y = self.evaluate(y, line, task, dispatcher)?.number(line)?;
                dispatcher.plot(
                    task,
                    plot_code,
                    graphics_coordinate(x, line)?,
                    graphics_coordinate(y, line)?,
                )?;
                Ok(Flow::Next)
            }
            Statement::Plot(code, x, y) => {
                let code = self
                    .evaluate(code, line, task, dispatcher)?
                    .number(line)?
                    .trunc() as u8;
                let x = self.evaluate(x, line, task, dispatcher)?.number(line)?;
                let y = self.evaluate(y, line, task, dispatcher)?.number(line)?;
                dispatcher.plot(
                    task,
                    code,
                    graphics_coordinate(x, line)?,
                    graphics_coordinate(y, line)?,
                )?;
                Ok(Flow::Next)
            }
            Statement::Gcol(action, colour) => {
                let action = self
                    .evaluate(action, line, task, dispatcher)?
                    .number(line)?
                    .trunc() as u8;
                let colour = self
                    .evaluate(colour, line, task, dispatcher)?
                    .number(line)?
                    .trunc() as u8;
                dispatcher.write_via_os_write_c(task, &[18, action, colour])?;
                Ok(Flow::Next)
            }
            Statement::If(condition, then_body, else_body) => {
                let selected = if self
                    .evaluate(condition, line, task, dispatcher)?
                    .truthy(line)?
                {
                    then_body
                } else {
                    else_body
                };
                for (index, nested) in selected.iter().enumerate() {
                    let return_depth = self.returns.len();
                    match self.execute_statement(nested, address, line, task, dispatcher)? {
                        Flow::Next => {}
                        flow @ Flow::Jump(_) => {
                            if self.returns.len() > return_depth {
                                // A nested IF may already have queued the
                                // remainder of its selected body. That inner
                                // remainder must run before this IF's siblings.
                                let mut continuation =
                                    std::mem::take(&mut self.inline_continuations);
                                continuation.extend(selected.iter().skip(index + 1).map(
                                    |statement| InlineContinuation {
                                        statement: statement.clone(),
                                        address,
                                        line,
                                    },
                                ));
                                self.inline_continuations = continuation;
                            }
                            return Ok(flow);
                        }
                        flow @ Flow::Stop => return Ok(flow),
                    }
                }
                Ok(Flow::Next)
            }
            Statement::IfBlock(condition) => {
                if self
                    .evaluate(condition, line, task, dispatcher)?
                    .truthy(line)?
                {
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
                    routine_name: None,
                    local_readonly_scope: false,
                    inline_continuation: VecDeque::new(),
                });
                Ok(Flow::Jump(destination))
            }
            Statement::Dim(declarations) => {
                for declaration in declarations {
                    self.dim(declaration, line, task, dispatcher)?;
                }
                Ok(Flow::Next)
            }
            Statement::Read(targets) => {
                for target in targets {
                    let Some((_, expression)) = self.data.get(self.data_cursor).cloned() else {
                        return Err(program_error(line, "READ passed the end of DATA"));
                    };
                    self.data_cursor += 1;
                    let value = self.evaluate(&expression, line, task, dispatcher)?;
                    self.assign(target, value, line, task, dispatcher)?;
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
            } => self.start_for(
                variable,
                start,
                end,
                step.as_ref(),
                address,
                line,
                task,
                dispatcher,
            ),
            Statement::Next(variable) => self.next_for(variable.as_deref(), address, line),
            Statement::Repeat => {
                self.repeat_loops.push(address + 1);
                Ok(Flow::Next)
            }
            Statement::Until(condition) => {
                let Some(repeat_address) = self.repeat_loops.last().copied() else {
                    return Err(program_error(line, "UNTIL has no matching REPEAT"));
                };
                if self
                    .evaluate(condition, line, task, dispatcher)?
                    .truthy(line)?
                {
                    self.repeat_loops.pop();
                    Ok(Flow::Next)
                } else {
                    Ok(Flow::Jump(repeat_address))
                }
            }
            Statement::ProcedureCall(name, arguments) => {
                #[cfg(feature = "experimental-jit")]
                if name.eq_ignore_ascii_case("IT") {
                    let values = self.evaluate_arguments(arguments, line, task, dispatcher)?;
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
                    return self
                        .execute_procedure_call_values(name, values, task.id, address, line);
                }

                let Some(definition) = self.program.procedures.get(name).cloned() else {
                    return Err(program_error(
                        line,
                        format!("unknown procedure PROC {name}"),
                    ));
                };
                let values = self.evaluate_arguments(arguments, line, task, dispatcher)?;
                self.enter_procedure(name, definition, values, task.id, address, line)
            }
            Statement::ImportedProcedureCall {
                module,
                name,
                arguments,
            } => {
                let values = self.evaluate_arguments(arguments, line, task, dispatcher)?;
                let symbol = name.to_ascii_uppercase();
                let (provider_id, resolved_symbol, provider) =
                    dispatcher.resolve_imported_basic64_symbol(&module, &symbol)?;
                if resolved_symbol.starts_with("FN:") {
                    return Err(program_error(
                        line,
                        format!("{}.{name} is a function, not a procedure", module),
                    ));
                }
                provider.invoke_imported_symbol(
                    provider_id,
                    &resolved_symbol,
                    values,
                    false,
                    task,
                    dispatcher,
                )?;
                Ok(Flow::Next)
            }
            Statement::LocalReadOnly {
                name,
                value_type,
                value,
            } => {
                if !self
                    .returns
                    .last()
                    .is_some_and(|frame| matches!(frame.kind, ReturnKind::Procedure))
                {
                    return Err(program_error(
                        line,
                        "LET READONLY must execute inside a PROC",
                    ));
                }
                let Some(scope) = self.local_readonly_scopes.last() else {
                    return Err(program_error(line, "read-only local scope is missing"));
                };
                if scope.contains_key(name) {
                    return Err(program_error(
                        line,
                        format!("read-only local {name} is declared more than once"),
                    ));
                }
                let value = self.evaluate(value, line, task, dispatcher)?;
                let value = normalize_system_arguments(
                    std::slice::from_ref(value_type),
                    vec![value],
                    line,
                    task.id,
                )?
                .into_iter()
                .next()
                .expect("one read-only local value");
                let previous = self.variables.insert(name.clone(), value);
                self.local_readonly_scopes
                    .last_mut()
                    .expect("read-only scope was checked above")
                    .insert(name.clone(), previous);
                Ok(Flow::Next)
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
                flags,
            } => {
                let swi_name = String::from_utf8_lossy(name).to_ascii_uppercase();
                let mut context = SwiContext::default();
                for (register, argument) in arguments.iter().enumerate().take(10) {
                    if let Some(argument) = argument {
                        let value = self.evaluate(argument, line, task, dispatcher)?;
                        context.registers[register] = value_to_sys_register(&value, line, task.id)?;
                    }
                }
                if swi_name == "OS_READC"
                    || (swi_name == "OS_BYTE" && matches!(context.registers[0] & 255, 21 | 129))
                {
                    if let Some(key) = self.pending_key.take() {
                        dispatcher.restore_polled_key(key);
                    }
                }
                dispatcher.dispatch_named_swi(&swi_name, task, &mut context)?;
                for (register, target) in results.iter().enumerate() {
                    self.set_variable(
                        target,
                        Value::Number(f64::from(context.registers[register] as i32)),
                        line,
                    )?;
                }
                if let Some(target) = flags {
                    self.set_variable(
                        target,
                        Value::Number(f64::from(context.returned_flags())),
                        line,
                    )?;
                }
                Ok(Flow::Next)
            }
            Statement::PrimitiveCall {
                name,
                arguments,
                results,
            } => {
                let signature = dispatcher.module_primitive_signature(&name)?;
                let mut context = SwiContext::default();
                for (register, argument) in arguments.iter().enumerate().take(10) {
                    if let Some(argument) = argument {
                        let value = self.evaluate(argument, line, task, dispatcher)?;
                        context.registers[register] = value_to_primitive_register(
                            &value,
                            &signature.arguments[register],
                            task.id,
                            line,
                        )?;
                    }
                }
                dispatcher.call_module_primitive(&name, task, &mut context)?;
                for (register, target) in results.iter().enumerate() {
                    let value = if target == "CARRY%" {
                        Value::Number(f64::from(u8::from(context.carry)))
                    } else {
                        match signature.results.get(register) {
                            Some(crate::ricochet::RegisterKind::Unsigned { bits: 32 }) => {
                                Value::Integer(i128::from(context.registers[register]))
                            }
                            Some(crate::ricochet::RegisterKind::OpaqueHandle { type_name }) => {
                                Value::Handle {
                                    type_name: type_name.to_ascii_uppercase(),
                                    raw: context.registers[register],
                                }
                            }
                            Some(crate::ricochet::RegisterKind::LogicalAddress { .. }) => {
                                Value::LogicalAddress {
                                    owner_task: task.id,
                                    raw: context.registers[register],
                                }
                            }
                            Some(crate::ricochet::RegisterKind::Signed { bits }) => {
                                let raw = context.registers[register];
                                let signed = match *bits {
                                    1..=31 => {
                                        let shift = 32 - u32::from(*bits);
                                        ((raw << shift) as i32) >> shift
                                    }
                                    _ => raw as i32,
                                };
                                Value::Number(f64::from(signed))
                            }
                            _ => Value::Number(f64::from(context.registers[register])),
                        }
                    };
                    self.set_variable(target, value, line)?;
                }
                Ok(Flow::Next)
            }
            Statement::Try => {
                let region =
                    self.try_regions.get(&address).cloned().ok_or_else(|| {
                        program_error(line, "TRY has no matching CATCH and ENDTRY")
                    })?;
                self.try_frames.push(ActiveTry {
                    catch_address: region.catch_address,
                    end_address: region.end_address,
                    error_name: region.error_name,
                    error_type: region.error_type,
                    return_depth: self.returns.len(),
                    for_depth: self.for_loops.len(),
                    repeat_depth: self.repeat_loops.len(),
                });
                Ok(Flow::Next)
            }
            Statement::Catch { .. } => {
                if let Some(frame) = self.try_frames.last()
                    && frame.catch_address == address
                {
                    let end_address = frame.end_address;
                    self.try_frames.pop();
                    return Ok(Flow::Jump(end_address + 1));
                }
                Err(program_error(line, "CATCH does not match an active TRY"))
            }
            Statement::EndTry => {
                if self.completed_try_ends.remove(&address) {
                    return Ok(Flow::Next);
                }
                if self
                    .try_frames
                    .last()
                    .is_some_and(|frame| frame.end_address == address)
                {
                    self.try_frames.pop();
                    Ok(Flow::Next)
                } else {
                    Err(program_error(line, "ENDTRY does not match an active TRY"))
                }
            }
            Statement::Throw {
                error_type,
                code,
                message,
            } => {
                let routine = self
                    .returns
                    .last()
                    .and_then(|frame| frame.routine_name.as_deref())
                    .ok_or_else(|| program_error(line, "THROW must occur inside a PROC or FN"))?;
                if !self
                    .program
                    .throws_types
                    .get(routine)
                    .is_some_and(|declared| declared.eq_ignore_ascii_case(error_type))
                {
                    return Err(program_error(
                        line,
                        format!("{routine} must declare THROWS {error_type}"),
                    ));
                }
                let code = self
                    .evaluate(code, line, task, dispatcher)?
                    .number(line)?
                    .trunc();
                if !(0.0..=f64::from(u32::MAX)).contains(&code) {
                    return Err(program_error(
                        line,
                        "structured error code is outside UINT32 range",
                    ));
                }
                let message = String::from_utf8_lossy(
                    self.evaluate(message, line, task, dispatcher)?
                        .string(line)?,
                )
                .into_owned();
                Err(RuntimeError::Structured {
                    type_name: error_type.to_ascii_uppercase(),
                    code: code as u32,
                    message,
                })
            }
            Statement::Call(expression) => {
                let address = self.address_value(expression, line, task, dispatcher)?;
                let mut context = SwiContext::default();
                for (register, name) in ["A%", "X%", "Y%"].into_iter().enumerate() {
                    context.registers[register] =
                        self.get_variable(name).number(line)? as i32 as u32;
                }
                context.carry = (self.get_variable("C%").number(line)? as i32 & 1) != 0;
                if address == 0xFFE0
                    || (address == 0xFFF4 && matches!(context.registers[0] & 255, 21 | 129))
                {
                    if let Some(key) = self.pending_key.take() {
                        dispatcher.restore_polled_key(key);
                    }
                }
                dispatcher
                    .dispatch_mos_call(address, task, &mut context)
                    .map_err(|error| program_error(line, error.to_string()))?;
                Ok(Flow::Next)
            }
            Statement::StarCommand(command) => {
                let command = String::from_utf8_lossy(command);
                let command_address = GUEST_MEMORY_BASE + 0x6000;
                task.memory
                    .write_bytes(command_address, command.as_bytes())?;
                task.memory
                    .write_byte(command_address + command.len() as u32, 0)?;
                let mut context = SwiContext::default();
                context.registers[0] = command_address;
                dispatcher.dispatch(OS_CLI, task, &mut context)?;
                Ok(Flow::Next)
            }
        }
    }

    fn jump_to_line(&self, target: u16, line: u16) -> Result<Flow, RuntimeError> {
        Ok(Flow::Jump(self.line_address(target, line)?))
    }

    fn handle_runtime_error(
        &mut self,
        error: &RuntimeError,
        line: u16,
        task_id: u64,
    ) -> Result<Option<usize>, RuntimeError> {
        while let Some(frame) = self.try_frames.pop() {
            let (actual_type, code, message) = match error {
                RuntimeError::Structured {
                    type_name,
                    code,
                    message,
                } => {
                    if !type_name.eq_ignore_ascii_case(&frame.error_type) {
                        continue;
                    }
                    (type_name.clone(), *code, message.as_bytes().to_vec())
                }
                RuntimeError::StandardErrorBlock { code, message } => {
                    let type_name = "OSError";
                    if !type_name.eq_ignore_ascii_case(&frame.error_type) {
                        continue;
                    }
                    (type_name.into(), *code, message.as_bytes().to_vec())
                }
                RuntimeError::InvalidSwi(number) => (
                    frame.error_type.clone(),
                    2,
                    format!("unsupported SWI &{number:02X}").into_bytes(),
                ),
                RuntimeError::EndOfInput => (frame.error_type.clone(), 3, b"end of input".to_vec()),
                RuntimeError::Memory(memory) => {
                    (frame.error_type.clone(), 5, memory.to_string().into_bytes())
                }
                RuntimeError::Io(io) => (frame.error_type.clone(), 4, io.to_string().into_bytes()),
                RuntimeError::Program(message) => {
                    (frame.error_type.clone(), 1, message.as_bytes().to_vec())
                }
            };
            let value = self.make_error_value(&actual_type, code, message, line, task_id)?;
            // A caught error can leave nested procedures and loop frames
            // behind. Restore their locals before resuming at CATCH so it has
            // the same observable state as ordinary procedure unwinding.
            while self.returns.len() > frame.return_depth {
                if let Some(return_frame) = self.returns.pop() {
                    if return_frame.local_readonly_scope {
                        self.restore_local_readonly_scope();
                    }
                    self.restore_variables(return_frame.saved_variables);
                }
            }
            self.inline_continuations.clear();
            self.for_loops.truncate(frame.for_depth);
            self.repeat_loops.truncate(frame.repeat_depth);
            self.set_variable(&frame.error_name, value, line)?;
            self.completed_try_ends.insert(frame.end_address);
            return Ok(Some(frame.catch_address + 1));
        }
        Ok(None)
    }

    fn make_error_value(
        &self,
        type_name: &str,
        code: u32,
        message: Vec<u8>,
        line: u16,
        task_id: u64,
    ) -> Result<Value, RuntimeError> {
        let Some(super::parser::SystemTypeDefinition::Error { fields: schema }) =
            self.program.system_types.get(type_name)
        else {
            return Err(program_error(
                line,
                format!("{type_name} is not a structured error type"),
            ));
        };
        let mut fields = HashMap::new();
        let mut readonly_fields = std::collections::HashSet::new();
        for field in schema {
            let field_value = match field.name.as_str() {
                "CODE" => Value::Number(f64::from(code)),
                "MESSAGE" => Value::String(message.clone()),
                _ => default_for_system_type(
                    &field.value_type,
                    &self.program.system_types,
                    line,
                    task_id,
                )?,
            };
            if field.read_only {
                readonly_fields.insert(field.name.clone());
            }
            fields.insert(field.name.clone(), field_value);
        }
        Ok(Value::Error {
            type_name: type_name.to_owned(),
            code,
            message,
            fields,
            readonly_fields,
        })
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
        task_id: u64,
        address: usize,
        line: u16,
    ) -> Result<Flow, RuntimeError> {
        let Some(definition) = self.program.procedures.get(name).cloned() else {
            return Err(program_error(
                line,
                format!("unknown procedure PROC {name}"),
            ));
        };
        self.enter_procedure(name, definition, values, task_id, address, line)
    }

    fn enter_procedure(
        &mut self,
        name: &str,
        definition: super::parser::Definition,
        values: Vec<Value>,
        task_id: u64,
        address: usize,
        line: u16,
    ) -> Result<Flow, RuntimeError> {
        if values.len() != definition.parameters.len() {
            return Err(program_error(
                line,
                format!("PROC {name} argument count mismatch"),
            ));
        }
        let values = if let Some(types) = self.program.typed_parameters.get(name) {
            normalize_system_arguments(types, values, line, task_id)?
        } else {
            values
        };
        let saved_variables = self.bind_parameters(&definition.parameters, values, line)?;
        self.local_readonly_scopes.push(HashMap::new());
        self.returns.push(ReturnFrame {
            kind: ReturnKind::Procedure,
            address: address + 1,
            saved_variables,
            routine_name: Some(name.to_ascii_uppercase()),
            local_readonly_scope: true,
            inline_continuation: VecDeque::new(),
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
        self.inline_continuations = frame.inline_continuation;
        if frame.local_readonly_scope {
            self.restore_local_readonly_scope();
        }
        self.restore_variables(frame.saved_variables);
        Ok(Flow::Jump(frame.address))
    }

    fn restore_local_readonly_scope(&mut self) {
        if let Some(scope) = self.local_readonly_scopes.pop() {
            self.restore_variables(scope.into_iter().collect());
        }
    }

    fn evaluate_arguments(
        &mut self,
        arguments: &[Expr],
        line: u16,
        task: &mut Task,
        dispatcher: &mut SwiDispatcher,
    ) -> Result<Vec<Value>, RuntimeError> {
        arguments
            .iter()
            .map(|argument| self.evaluate(argument, line, task, dispatcher))
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

    fn function_body(
        &self,
        definition: &super::parser::Definition,
        name: &str,
        line: u16,
    ) -> Result<(Vec<(String, super::parser::SystemType, Expr)>, Expr), RuntimeError> {
        let mut local_readonly = Vec::new();
        for instruction in self.program.instructions.iter().skip(definition.entry) {
            match &instruction.statement {
                Statement::LocalReadOnly {
                    name,
                    value_type,
                    value,
                } if self.program.options.mode == crate::configure::BasicLanguageMode::Basic64 => {
                    local_readonly.push((name.clone(), value_type.clone(), value.clone()));
                }
                Statement::NoOp => {}
                Statement::Assign(target, _) => {
                    let target_name = match target {
                        LValue::Variable(name)
                        | LValue::ArrayElement(name, _)
                        | LValue::StringSlice(name, _, _)
                        | LValue::RecordField(name, _) => Some(name.as_str()),
                        LValue::RecordPath(path) => path.first().map(String::as_str),
                        LValue::Memory(_, _)
                        | LValue::MemoryByteAt(_, _)
                        | LValue::MemoryOffset(_, _, _)
                        | LValue::MemoryString(_) => None,
                    };
                    if let Some((local_name, _, _)) = local_readonly.iter().find(|(name, _, _)| {
                        target_name.is_some_and(|target| target.eq_ignore_ascii_case(name))
                    }) {
                        return Err(program_error(
                            instruction.line_number,
                            format!("{local_name} is a read-only local binding"),
                        ));
                    }
                    if self.program.options.mode == crate::configure::BasicLanguageMode::Basic64 {
                        return Err(program_error(
                            instruction.line_number,
                            format!(
                                "FN {name} supports only LET READONLY declarations and an expression result"
                            ),
                        ));
                    }
                }
                Statement::FunctionReturn(expression) => {
                    return Ok((local_readonly, expression.clone()));
                }
                Statement::DefineFunction(_, _) | Statement::DefineProcedure(_, _) => break,
                _ if self.program.options.mode == crate::configure::BasicLanguageMode::Basic64 => {
                    return Err(program_error(
                        instruction.line_number,
                        format!(
                            "FN {name} supports only LET READONLY declarations and an expression result"
                        ),
                    ));
                }
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
        dispatcher: &mut SwiDispatcher,
    ) -> Result<Flow, RuntimeError> {
        let start_value = self.evaluate(start, line, task, dispatcher)?;
        let end_value = self.evaluate(end, line, task, dispatcher)?;
        let step_value = match step {
            Some(expression) => self.evaluate(expression, line, task, dispatcher)?,
            None if exact_integer(&start_value).is_some()
                && exact_integer(&end_value).is_some() =>
            {
                Value::Integer(1)
            }
            None => Value::Number(1.0),
        };
        let exact_start_end = exact_integer_pair(&start_value, &end_value, line)?;
        let exact_start_step = exact_integer_pair(&start_value, &step_value, line)?;
        let exact_end_step = exact_integer_pair(&end_value, &step_value, line)?;
        let exact_loop = if let (
            Some((start_number, end_number, start_end_kind)),
            Some((_, step_number, start_step_kind)),
            Some((_, _, end_step_kind)),
        ) = (exact_start_end, exact_start_step, exact_end_step)
        {
            if start_end_kind != start_step_kind || start_end_kind != end_step_kind {
                return Err(program_error(
                    line,
                    "FOR bounds and STEP use incompatible 64-bit integer types",
                ));
            }
            Some((start_number, end_number, step_number))
        } else {
            None
        };
        if let Some((_, _, step_number)) = exact_loop {
            if step_number == 0 {
                return Err(program_error(line, "FOR STEP cannot be zero"));
            }
        } else if step_value.number(line)? == 0.0 {
            return Err(program_error(line, "FOR STEP cannot be zero"));
        }
        let next_address = self
            .for_pairs
            .get(&address)
            .copied()
            .ok_or_else(|| program_error(line, "FOR has no matching NEXT"))?;
        self.set_variable(variable, start_value.clone(), line)?;

        let enters_loop = if let Some((start, end, step)) = exact_loop {
            if step > 0 { start <= end } else { start >= end }
        } else {
            let start = start_value.number(line)?;
            let end = end_value.number(line)?;
            let step = step_value.number(line)?;
            if step > 0.0 {
                start <= end
            } else {
                start >= end
            }
        };
        if !enters_loop {
            return Ok(Flow::Jump(next_address + 1));
        }

        self.for_loops.push(ForFrame {
            variable: variable.to_owned(),
            limit: if exact_loop.is_some() {
                end_value
            } else {
                Value::Number(end_value.number(line)?)
            },
            step: if exact_loop.is_some() {
                step_value
            } else {
                Value::Number(step_value.number(line)?)
            },
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
        let step = frame.step.clone();
        let limit = frame.limit.clone();
        let body_address = frame.body_address;
        let current = self.get_variable(&variable);
        let exact = exact_integer_pair(&current, &step, line)?;
        let (next_value, continues) = if let Some((current, step, kind)) = exact {
            let next = current
                .checked_add(step)
                .ok_or_else(|| program_error(line, "FOR counter arithmetic overflowed"))?;
            let value = exact_integer_result(kind, next, line)?;
            let (_, limit, _) = exact_integer_pair(&value, &limit, line)?
                .ok_or_else(|| program_error(line, "FOR counter changed numeric type"))?;
            let step_is_positive = step > 0;
            (
                value,
                if step_is_positive {
                    next <= limit
                } else {
                    next >= limit
                },
            )
        } else {
            let current = current.number(line)?;
            let step = step.number(line)?;
            let limit = limit.number(line)?;
            let next = current + step;
            (
                Value::Number(next),
                if step > 0.0 {
                    next <= limit
                } else {
                    next >= limit
                },
            )
        };
        self.set_variable(&variable, next_value, line)?;
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
        dispatcher: &mut SwiDispatcher,
    ) -> Result<(), RuntimeError> {
        if declaration.dimensions.is_empty() {
            self.variables
                .insert(declaration.name.clone(), default_value(&declaration.name));
            return Ok(());
        }
        let mut length = 1_usize;
        for dimension in &declaration.dimensions {
            let upper_bound = self
                .evaluate(dimension, line, task, dispatcher)?
                .number(line)?;
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
        task: &mut Task,
        dispatcher: &mut SwiDispatcher,
    ) -> Result<Value, RuntimeError> {
        self.interpreted_expression_count = self.interpreted_expression_count.saturating_add(1);
        match expression {
            Expr::Number(value) => Ok(Value::Number(*value)),
            Expr::Integer(value)
                if self.program.options.mode == crate::configure::BasicLanguageMode::Basic64 =>
            {
                Ok(Value::Integer(*value))
            }
            // Classic and Hybrid retain the historic floating BASIC number
            // model. Exact integral literals are a BASIC64-only facility.
            Expr::Integer(value) => Ok(Value::Number(*value as f64)),
            Expr::String(value) => Ok(Value::String(value.clone())),
            Expr::Variable(name) if name == "TIME" => {
                Ok(Value::Number(f64::from(self.clock.read() as u32 as i32)))
            }
            Expr::Variable(name) => Ok(self.get_variable(name)),
            Expr::ArrayElement(name, index) => {
                let index = self.evaluate(index, line, task, dispatcher)?.number(line)?;
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
                let operand = self.evaluate(operand, line, task, dispatcher)?;
                if let Some((value, kind)) = exact_integer(&operand) {
                    let result = match operator {
                        UnaryOp::Plus => value,
                        UnaryOp::Minus => value
                            .checked_neg()
                            .ok_or_else(|| program_error(line, "integer negation overflowed"))?,
                        UnaryOp::Not => match kind {
                            ExactIntegerKind::Unsigned64 => i128::from(!(value as u64)),
                            ExactIntegerKind::Signed64 => i128::from(!(value as i64)),
                            ExactIntegerKind::Literal => !value,
                        },
                    };
                    return exact_integer_result(kind, result, line);
                }
                let operand = operand.number(line)?;
                let value = match operator {
                    UnaryOp::Plus => operand,
                    UnaryOp::Minus => -operand,
                    UnaryOp::Not => f64::from(!self.basic_bitwise_i32(operand)),
                };
                Ok(Value::Number(value))
            }
            Expr::Binary(left, operator, right) => {
                let left = self.evaluate(left, line, task, dispatcher)?;
                let right = self.evaluate(right, line, task, dispatcher)?;
                self.evaluate_binary(left, *operator, right, line, task.id)
            }
            Expr::Builtin(token, arguments) => {
                self.evaluate_builtin(*token, arguments, line, task, dispatcher)
            }
            Expr::UserFunction(name, arguments) => {
                let values = self.evaluate_arguments(arguments, line, task, dispatcher)?;
                let Some(definition) = self.program.functions.get(name).cloned() else {
                    if !values.is_empty() {
                        return Err(program_error(line, format!("unknown function FN {name}")));
                    }
                    return match self.program.system_types.get(name) {
                        Some(super::parser::SystemTypeDefinition::Record { .. }) => {
                            default_for_system_type(
                                &super::parser::SystemType::Record(name.clone()),
                                &self.program.system_types,
                                line,
                                task.id,
                            )
                        }
                        Some(
                            super::parser::SystemTypeDefinition::Enum { .. }
                            | super::parser::SystemTypeDefinition::Flags { .. },
                        ) => Err(program_error(line, "enum and flag values use Type.Member")),
                        Some(super::parser::SystemTypeDefinition::Handle) => Err(program_error(
                            line,
                            "opaque handles can only come from a typed capability result",
                        )),
                        Some(super::parser::SystemTypeDefinition::Error { .. }) => {
                            Err(program_error(
                                line,
                                "structured errors can only be produced by THROW or a failing service",
                            ))
                        }
                        None => Err(program_error(line, format!("unknown function FN {name}"))),
                    };
                };
                if values.len() != definition.parameters.len() {
                    return Err(program_error(
                        line,
                        format!("FN {name} argument count mismatch"),
                    ));
                }
                let values = if let Some(types) = self.program.typed_parameters.get(name) {
                    normalize_system_arguments(types, values, line, task.id)?
                } else {
                    values
                };
                let (local_readonly, expression) = self.function_body(&definition, name, line)?;
                let saved = self.bind_parameters(&definition.parameters, values, line)?;
                self.local_readonly_scopes.push(HashMap::new());
                let result = (|| {
                    for (local_name, local_type, initializer) in local_readonly {
                        let value = self.evaluate(&initializer, line, task, dispatcher)?;
                        let value = normalize_system_arguments(
                            std::slice::from_ref(&local_type),
                            vec![value],
                            line,
                            task.id,
                        )?
                        .into_iter()
                        .next()
                        .expect("one read-only local value");
                        let previous = self.variables.insert(local_name.clone(), value);
                        self.local_readonly_scopes
                            .last_mut()
                            .expect("function read-only scope was just created")
                            .insert(local_name, previous);
                    }
                    self.evaluate(&expression, line, task, dispatcher)
                })();
                self.restore_local_readonly_scope();
                self.restore_variables(saved.clone());
                let value = match result {
                    Ok(value) => value,
                    Err(RuntimeError::Structured {
                        type_name,
                        code,
                        message,
                    }) => {
                        let Some(expected) = self.program.throws_types.get(name) else {
                            return Err(program_error(
                                line,
                                format!(
                                    "FN {name} raised {type_name} without a THROWS declaration"
                                ),
                            ));
                        };
                        if !expected.eq_ignore_ascii_case(&type_name) {
                            return Err(program_error(
                                line,
                                format!(
                                    "FN {name} declared THROWS {expected} but raised {type_name}"
                                ),
                            ));
                        }
                        return Err(RuntimeError::Structured {
                            type_name,
                            code,
                            message,
                        });
                    }
                    Err(error) => {
                        if let Some(expected) = self.program.throws_types.get(name) {
                            let code = match &error {
                                RuntimeError::EndOfInput => 3,
                                RuntimeError::InvalidSwi(_) => 2,
                                RuntimeError::Memory(_) => 5,
                                RuntimeError::Io(_) => 4,
                                RuntimeError::Program(_) => 1,
                                RuntimeError::Structured { .. } => unreachable!(),
                                RuntimeError::StandardErrorBlock { code, .. } => *code,
                            };
                            return Err(RuntimeError::Structured {
                                type_name: expected.clone(),
                                code,
                                message: error.to_string(),
                            });
                        }
                        return Err(error);
                    }
                };
                if let Some(result_type) = self.program.typed_results.get(name) {
                    Ok(normalize_system_arguments(
                        std::slice::from_ref(result_type),
                        vec![value],
                        line,
                        task.id,
                    )?
                    .pop()
                    .expect("one typed function result"))
                } else {
                    Ok(value)
                }
            }
            Expr::ImportedFunction {
                module,
                name,
                arguments,
            } => {
                let values = self.evaluate_arguments(arguments, line, task, dispatcher)?;
                let symbol = format!("FN:{}", name.to_ascii_uppercase());
                let (provider_id, resolved_symbol, provider) =
                    dispatcher.resolve_imported_basic64_symbol(&module, &symbol)?;
                if !resolved_symbol.starts_with("FN:") {
                    return Err(program_error(
                        line,
                        format!("{}.{name} is a procedure, not a function", module),
                    ));
                }
                provider
                    .invoke_imported_symbol(
                        provider_id,
                        &resolved_symbol,
                        values,
                        true,
                        task,
                        dispatcher,
                    )?
                    .ok_or_else(|| program_error(line, "imported function returned no result"))
            }
            Expr::MemoryRead(width, address) => {
                let address = self.address_value(address, line, task, dispatcher)?;
                self.read_memory(*width, address, line, task)
            }
            Expr::Member(base, field) => {
                if let Expr::Variable(type_name) = base.as_ref() {
                    match self.program.system_types.get(type_name) {
                        Some(super::parser::SystemTypeDefinition::Enum { members, .. }) => {
                            let value = members.get(field).ok_or_else(|| {
                                program_error(
                                    line,
                                    format!("unknown enum member {type_name}.{field}"),
                                )
                            })?;
                            return Ok(Value::Enum {
                                type_name: type_name.clone(),
                                value: *value,
                            });
                        }
                        Some(super::parser::SystemTypeDefinition::Flags { members, .. }) => {
                            let value = members.get(field).ok_or_else(|| {
                                program_error(
                                    line,
                                    format!("unknown flag member {type_name}.{field}"),
                                )
                            })?;
                            return Ok(Value::Flags {
                                type_name: type_name.clone(),
                                value: *value as u64,
                            });
                        }
                        _ => {}
                    }
                }
                self.evaluate(base, line, task, dispatcher)?
                    .record_field(field, line)
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
        task_id: u64,
    ) -> Result<Value, RuntimeError> {
        if let Some((left_exact, right_exact, kind)) = exact_integer_pair(&left, &right, line)? {
            if matches!(
                operator,
                BinaryOp::Equal
                    | BinaryOp::NotEqual
                    | BinaryOp::Less
                    | BinaryOp::LessEqual
                    | BinaryOp::Greater
                    | BinaryOp::GreaterEqual
            ) {
                let result = match operator {
                    BinaryOp::Equal => left_exact == right_exact,
                    BinaryOp::NotEqual => left_exact != right_exact,
                    BinaryOp::Less => left_exact < right_exact,
                    BinaryOp::LessEqual => left_exact <= right_exact,
                    BinaryOp::Greater => left_exact > right_exact,
                    BinaryOp::GreaterEqual => left_exact >= right_exact,
                    _ => unreachable!(),
                };
                return Ok(Value::Number(if result { 1.0 } else { 0.0 }));
            }
            let result = match operator {
                BinaryOp::Add => left_exact.checked_add(right_exact),
                BinaryOp::Subtract => left_exact.checked_sub(right_exact),
                BinaryOp::Multiply => left_exact.checked_mul(right_exact),
                BinaryOp::IntegerDivide => {
                    if right_exact == 0 {
                        return Err(program_error(line, "integer division by zero"));
                    }
                    left_exact.checked_div(right_exact)
                }
                BinaryOp::Modulo => {
                    if right_exact == 0 {
                        return Err(program_error(line, "MOD by zero"));
                    }
                    left_exact.checked_rem(right_exact)
                }
                BinaryOp::And => Some(match kind {
                    ExactIntegerKind::Unsigned64 => {
                        i128::from((left_exact as u64) & (right_exact as u64))
                    }
                    ExactIntegerKind::Signed64 => {
                        i128::from((left_exact as i64) & (right_exact as i64))
                    }
                    ExactIntegerKind::Literal => left_exact & right_exact,
                }),
                BinaryOp::Or => Some(match kind {
                    ExactIntegerKind::Unsigned64 => {
                        i128::from((left_exact as u64) | (right_exact as u64))
                    }
                    ExactIntegerKind::Signed64 => {
                        i128::from((left_exact as i64) | (right_exact as i64))
                    }
                    ExactIntegerKind::Literal => left_exact | right_exact,
                }),
                BinaryOp::ShiftLeft => {
                    let shift = u32::try_from(right_exact)
                        .map_err(|_| program_error(line, "shift count must be nonnegative"))?;
                    Some(match kind {
                        ExactIntegerKind::Unsigned64 => {
                            i128::from((left_exact as u64).wrapping_shl(shift & 63))
                        }
                        ExactIntegerKind::Signed64 => {
                            i128::from((left_exact as i64).wrapping_shl(shift & 63))
                        }
                        ExactIntegerKind::Literal => left_exact.wrapping_shl(shift),
                    })
                }
                BinaryOp::Divide | BinaryOp::Power => None,
                BinaryOp::Equal
                | BinaryOp::NotEqual
                | BinaryOp::Less
                | BinaryOp::LessEqual
                | BinaryOp::Greater
                | BinaryOp::GreaterEqual => unreachable!(),
            };
            if let Some(result) = result {
                return exact_integer_result(kind, result, line);
            }
            if kind != ExactIntegerKind::Literal
                && matches!(operator, BinaryOp::Divide | BinaryOp::Power)
            {
                return Err(program_error(
                    line,
                    "floating arithmetic on a 64-bit integer requires an explicit conversion",
                ));
            }
        } else if matches!(
            (&left, &right),
            (Value::Int64(_) | Value::UInt64(_), Value::Number(_))
                | (Value::Number(_), Value::Int64(_) | Value::UInt64(_))
        ) {
            return Err(program_error(
                line,
                "mixing 64-bit integers with floating values requires an explicit conversion",
            ));
        }
        if matches!(operator, BinaryOp::Or | BinaryOp::And)
            && let (
                Value::Flags {
                    type_name: left_type,
                    value: left_value,
                },
                Value::Flags {
                    type_name: right_type,
                    value: right_value,
                },
            ) = (&left, &right)
        {
            if left_type != right_type {
                return Err(program_error(
                    line,
                    "bitwise flag operations require values of the same FLAGS type",
                ));
            }
            return Ok(Value::Flags {
                type_name: left_type.clone(),
                value: if operator == BinaryOp::Or {
                    left_value | right_value
                } else {
                    left_value & right_value
                },
            });
        }
        if let Value::LogicalAddress { owner_task, raw } = &left
            && matches!(operator, BinaryOp::Add | BinaryOp::Subtract)
            && let Some(offset) = address_offset_value(&right, line)?
        {
            if *owner_task != task_id {
                return Err(program_error(
                    line,
                    "logical address belongs to a different caller task",
                ));
            }
            let signed_offset = if operator == BinaryOp::Add {
                offset
            } else {
                offset
                    .checked_neg()
                    .ok_or_else(|| program_error(line, "logical address arithmetic overflowed"))?
            };
            let next = i64::from(*raw)
                .checked_add(signed_offset)
                .filter(|value| (0..=i64::from(u32::MAX)).contains(value))
                .ok_or_else(|| program_error(line, "logical address arithmetic overflowed"))?;
            return Ok(Value::LogicalAddress {
                owner_task: *owner_task,
                raw: next as u32,
            });
        }
        if let (
            Value::LogicalAddress {
                owner_task: left_owner,
                raw: left_raw,
            },
            BinaryOp::Subtract,
            Value::LogicalAddress {
                owner_task: right_owner,
                raw: right_raw,
            },
        ) = (&left, operator, &right)
        {
            if left_owner != right_owner {
                return Err(program_error(
                    line,
                    "logical addresses belong to different caller tasks",
                ));
            }
            if *left_owner != task_id {
                return Err(program_error(
                    line,
                    "logical address belongs to a different caller task",
                ));
            }
            return Ok(Value::Number(f64::from(*left_raw) - f64::from(*right_raw)));
        }
        let (left, right) = coerce_exact_literal_with_float(left, right, line)?;
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
                (
                    Value::LogicalAddress {
                        owner_task: left_owner,
                        raw: left,
                    },
                    Value::LogicalAddress {
                        owner_task: right_owner,
                        raw: right,
                    },
                ) if left_owner == right_owner && *left_owner == task_id => left.cmp(right),
                (
                    Value::Enum {
                        type_name: left_type,
                        value: left,
                    },
                    Value::Enum {
                        type_name: right_type,
                        value: right,
                    },
                ) if left_type == right_type => left.cmp(right),
                (
                    Value::Flags {
                        type_name: left_type,
                        value: left,
                    },
                    Value::Flags {
                        type_name: right_type,
                        value: right,
                    },
                ) if left_type == right_type => left.cmp(right),
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
            BinaryOp::And => {
                f64::from(self.basic_bitwise_i32(left) & self.basic_bitwise_i32(right))
            }
            BinaryOp::Or => f64::from(self.basic_bitwise_i32(left) | self.basic_bitwise_i32(right)),
            BinaryOp::ShiftLeft => f64::from(
                self.basic_bitwise_i32(left)
                    .wrapping_shl((right as u32) & 31),
            ),
            BinaryOp::Equal
            | BinaryOp::NotEqual
            | BinaryOp::Less
            | BinaryOp::LessEqual
            | BinaryOp::Greater
            | BinaryOp::GreaterEqual => unreachable!(),
        };
        Ok(Value::Number(value))
    }

    fn basic_bitwise_i32(&self, value: f64) -> i32 {
        if self.program.options.mode == crate::configure::BasicLanguageMode::Basic64
            && (0.0..=f64::from(u32::MAX)).contains(&value)
            && value.fract() == 0.0
        {
            value as u32 as i32
        } else {
            value as i32
        }
    }

    fn evaluate_builtin(
        &mut self,
        token: u8,
        arguments: &[Expr],
        line: u16,
        task: &mut Task,
        dispatcher: &mut SwiDispatcher,
    ) -> Result<Value, RuntimeError> {
        let mut values = Vec::with_capacity(arguments.len());
        for argument in arguments {
            values.push(self.evaluate(argument, line, task, dispatcher)?);
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
            0x94 => {
                if values.len() != 1 {
                    return Err(program_error(line, "ABS expects one argument"));
                }
                match &values[0] {
                    Value::Integer(value) => value
                        .checked_abs()
                        .map(Value::Integer)
                        .ok_or_else(|| program_error(line, "ABS integer overflowed")),
                    Value::Int64(value) => value
                        .checked_abs()
                        .map(Value::Int64)
                        .ok_or_else(|| program_error(line, "ABS INT64 overflowed")),
                    Value::UInt64(_) => Ok(values[0].clone()),
                    value => Ok(Value::Number(value.number(line)?.abs())),
                }
            }
            0x9B => Ok(Value::Number(one_number()?.cos())),
            0xB5 => Ok(Value::Number(one_number()?.sin())),
            0xA8 => {
                if values.len() != 1 {
                    return Err(program_error(line, "INT expects one argument"));
                }
                match &values[0] {
                    Value::Integer(_) | Value::Int64(_) | Value::UInt64(_) => Ok(values[0].clone()),
                    value => Ok(Value::Number(value.number(line)?.floor())),
                }
            }
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
            0xC3 => {
                if values.len() != 1 {
                    return Err(program_error(line, "STR$ expects one argument"));
                }
                Ok(Value::String(values[0].number_string(line)?))
            }
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
        self.ensure_binding_writable(name, line)?;
        let value = if let Some(kind) = self.program.module_state_types.get(name) {
            normalize_system_arguments(std::slice::from_ref(kind), vec![value], line, 0)?
                .into_iter()
                .next()
                .expect("one module-state value")
        } else {
            value
        };
        if name == "TIME" {
            self.clock
                .set(value.number(line)?.trunc() as i32 as u32 as u64);
            return Ok(());
        }
        let value = if name.ends_with('$') {
            match value {
                Value::String(_) => value,
                _ => {
                    return Err(program_error(
                        line,
                        format!("{name} requires a string value"),
                    ));
                }
            }
        } else if name.ends_with('%') {
            // A register with an opaque-handle contract is represented in the
            // same BASIC register variable (R0%), but remains nominally typed.
            // Preserve that value only when the destination already carries
            // the same handle type; ordinary numeric variables cannot absorb
            // a handle or record value.
            if let Some(Value::Handle {
                type_name: expected,
                ..
            }) = self.variables.get(name)
            {
                match value {
                    Value::Handle { type_name, raw }
                        if type_name.eq_ignore_ascii_case(expected) =>
                    {
                        Value::Handle { type_name, raw }
                    }
                    _ => {
                        return Err(program_error(
                            line,
                            format!("{name} requires opaque handle {expected}"),
                        ));
                    }
                }
            } else if let Some(Value::LogicalAddress { owner_task, .. }) = self.variables.get(name)
            {
                match value {
                    Value::LogicalAddress {
                        owner_task: actual_owner,
                        raw,
                    } if actual_owner == *owner_task => Value::LogicalAddress {
                        owner_task: actual_owner,
                        raw,
                    },
                    Value::Integer(number) if (0..=i128::from(u32::MAX)).contains(&number) => {
                        Value::LogicalAddress {
                            owner_task: *owner_task,
                            raw: number as u32,
                        }
                    }
                    Value::Number(number)
                        if (0.0..=f64::from(u32::MAX)).contains(&number)
                            && number.fract() == 0.0 =>
                    {
                        Value::LogicalAddress {
                            owner_task: *owner_task,
                            raw: number as u32,
                        }
                    }
                    _ => {
                        return Err(program_error(
                            line,
                            format!("{name} requires a caller-scoped ADDRESS32"),
                        ));
                    }
                }
            } else if matches!(value, Value::LogicalAddress { .. })
                && self.program.options.mode == crate::configure::BasicLanguageMode::Basic64
            {
                value
            } else if matches!(
                value,
                Value::Integer(_) | Value::UInt64(_) | Value::Int64(_)
            ) && self.program.options.mode == crate::configure::BasicLanguageMode::Basic64
            {
                value
            } else {
                let number = value.number(line)?;
                Value::Number(f64::from(number.trunc() as i32))
            }
        } else if matches!(value, Value::Number(_)) {
            Value::Number(value.number(line)?)
        } else if self.program.options.mode == crate::configure::BasicLanguageMode::Basic64 {
            value
        } else {
            return Err(program_error(
                line,
                "legacy BASIC variables only accept numeric or string values",
            ));
        };
        self.variables.insert(name.to_owned(), value);
        if self.readonly_bindings.contains(name) {
            self.readonly_initialized.insert(name.to_owned());
        }
        Ok(())
    }

    fn ensure_binding_writable(&self, name: &str, line: u16) -> Result<(), RuntimeError> {
        if self
            .local_readonly_scopes
            .iter()
            .any(|scope| scope.contains_key(name))
        {
            return Err(program_error(
                line,
                format!("{name} is a read-only local binding"),
            ));
        }
        if self.readonly_bindings.contains(name) && self.readonly_initialized.contains(name) {
            return Err(program_error(
                line,
                format!("{name} is a read-only module binding"),
            ));
        }
        Ok(())
    }

    fn assign(
        &mut self,
        target: &LValue,
        value: Value,
        line: u16,
        task: &mut Task,
        dispatcher: &mut SwiDispatcher,
    ) -> Result<(), RuntimeError> {
        match target {
            LValue::Variable(name) => self.set_variable(name, value, line),
            LValue::RecordField(record_name, field_name) => {
                self.ensure_binding_writable(record_name, line)?;
                self.assign_record_path(
                    &[record_name.clone(), field_name.clone()],
                    value,
                    line,
                    task.id,
                )
            }
            LValue::RecordPath(path) => {
                if let Some(name) = path.first() {
                    self.ensure_binding_writable(name, line)?;
                }
                self.assign_record_path(path, value, line, task.id)
            }
            LValue::ArrayElement(name, index) => {
                self.ensure_binding_writable(name, line)?;
                let index = array_index(
                    self.evaluate(index, line, task, dispatcher)?.number(line)?,
                    line,
                )?;
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
                let address = self.address_value(address, line, task, dispatcher)?;
                self.write_memory(*width, address, value, line, task)
            }
            LValue::MemoryByteAt(base, offset) => {
                let base_value = self.evaluate(base, line, task, dispatcher)?;
                let base = self.guest_address(base_value, line, task)?;
                let offset = self
                    .evaluate(offset, line, task, dispatcher)?
                    .number(line)?;
                let address = checked_guest_address(f64::from(base) + offset, line)?;
                self.write_memory(MemoryWidth::Byte, address, value, line, task)
            }
            LValue::MemoryOffset(width, base, offset) => {
                let base_value = self.evaluate(base, line, task, dispatcher)?;
                let base = self.guest_address(base_value, line, task)?;
                let offset = self
                    .evaluate(offset, line, task, dispatcher)?
                    .number(line)?;
                let address = checked_guest_address(f64::from(base) + offset, line)?;
                self.write_memory(*width, address, value, line, task)
            }
            LValue::MemoryString(address) => {
                let address = self.address_value(address, line, task, dispatcher)?;
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
                self.ensure_binding_writable(name, line)?;
                let start = self.evaluate(start, line, task, dispatcher)?.number(line)?;
                let length = bounded_string_length(
                    self.evaluate(length, line, task, dispatcher)?
                        .number(line)?,
                    line,
                )?;
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
        task: &mut Task,
        dispatcher: &mut SwiDispatcher,
    ) -> Result<u32, RuntimeError> {
        let value = self.evaluate(expression, line, task, dispatcher)?;
        self.guest_address(value, line, task)
    }

    fn guest_address(&self, value: Value, line: u16, task: &Task) -> Result<u32, RuntimeError> {
        match value {
            Value::LogicalAddress { owner_task, raw } if owner_task == task.id => Ok(raw),
            Value::LogicalAddress { .. } => Err(program_error(
                line,
                "logical address belongs to a different caller task",
            )),
            Value::Number(number) => checked_guest_address(number, line),
            _ => Err(program_error(
                line,
                "expected a caller-scoped logical address",
            )),
        }
    }

    fn assign_record_path(
        &mut self,
        path: &[String],
        value: Value,
        line: u16,
        task_id: u64,
    ) -> Result<(), RuntimeError> {
        if path.len() < 2 {
            return Err(program_error(line, "record assignment requires a field"));
        }
        let mut type_name = match self.variables.get(&path[0]) {
            Some(Value::Record { type_name, .. }) | Some(Value::Error { type_name, .. }) => {
                type_name.clone()
            }
            _ => {
                return Err(program_error(
                    line,
                    format!("{} is not an initialized record", path[0]),
                ));
            }
        };
        let mut readonly = false;
        let mut declared_type = None;
        for (index, field_name) in path.iter().enumerate().skip(1) {
            let schema = self.program.system_types.get(&type_name).ok_or_else(|| {
                program_error(line, format!("record type {type_name} is missing"))
            })?;
            let fields = match schema {
                super::parser::SystemTypeDefinition::Record { fields }
                | super::parser::SystemTypeDefinition::Error { fields } => fields,
                _ => {
                    return Err(program_error(
                        line,
                        format!("{type_name} has no record fields"),
                    ));
                }
            };
            let field = fields
                .iter()
                .find(|field| field.name == *field_name)
                .ok_or_else(|| {
                    program_error(line, format!("unknown field {type_name}.{field_name}"))
                })?;
            if index + 1 == path.len() {
                readonly = field.read_only;
                declared_type = Some(field.value_type.clone());
            } else {
                type_name = match &field.value_type {
                    super::parser::SystemType::Record(name)
                    | super::parser::SystemType::Error(name) => name.clone(),
                    _ => {
                        return Err(program_error(
                            line,
                            format!("{} is not a nested record", field.name),
                        ));
                    }
                };
            }
        }
        if readonly {
            return Err(program_error(
                line,
                format!("{} is read-only", path.join(".")),
            ));
        }
        let value = normalize_system_arguments(
            std::slice::from_ref(&declared_type.expect("path includes a final field")),
            vec![value],
            line,
            task_id,
        )?
        .pop()
        .expect("one record field value");
        let Some(root) = self.variables.get_mut(&path[0]) else {
            return Err(program_error(
                line,
                format!("{} is not initialized", path[0]),
            ));
        };
        set_nested_record_field(root, &path[1..], value, line)
    }

    fn read_memory(
        &self,
        width: MemoryWidth,
        address: u32,
        line: u16,
        task: &Task,
    ) -> Result<Value, RuntimeError> {
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
                    let value = self.evaluate(expression, line, task, dispatcher)?;
                    let bytes = match value {
                        Value::String(value) => value,
                        Value::Number(value) => {
                            format_print_number(value, self.print_format).into_bytes()
                        }
                        Value::Integer(value) => value.to_string().into_bytes(),
                        Value::UInt64(value) => value.to_string().into_bytes(),
                        Value::Int64(value) => value.to_string().into_bytes(),
                        Value::Enum { value, .. } => format!("{value}").into_bytes(),
                        Value::Flags { value, .. } => format!("{value}").into_bytes(),
                        Value::Record { type_name, .. } => format!("<{type_name}>").into_bytes(),
                        Value::Handle { type_name, .. } => {
                            format!("<HANDLE {type_name}>").into_bytes()
                        }
                        Value::LogicalAddress { raw, .. } => {
                            format!("<ADDRESS32 &{raw:08X}>").into_bytes()
                        }
                        Value::Error { message, .. } => message,
                    };
                    self.emit(&bytes, task, dispatcher)?;
                }
                PrintItem::Spaces(expression) => {
                    let count = self
                        .evaluate(expression, line, task, dispatcher)?
                        .number(line)?;
                    let count = bounded_string_length(count, line)?;
                    self.emit(&vec![b' '; count], task, dispatcher)?;
                }
                PrintItem::Tab(x, y) => {
                    let x = self
                        .evaluate(x, line, task, dispatcher)?
                        .number(line)?
                        .trunc() as u8;
                    let y = self
                        .evaluate(y, line, task, dispatcher)?
                        .number(line)?
                        .trunc() as u8;
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
}

pub(super) fn invoke_system_definition(
    mut program: ParsedProgram,
    definition_name: &str,
    module_id: crate::ricochet::ModuleId,
    contract: &crate::ricochet::SwiContract,
    workspace: &ModuleWorkspace,
    persistent_state: &std::collections::BTreeMap<String, super::parser::SystemType>,
    task: &mut Task,
    dispatcher: &mut SwiDispatcher,
    context: &mut SwiContext,
) -> Result<(), RuntimeError> {
    let definition_name = definition_name.to_ascii_uppercase();
    if !program.procedures.contains_key(&definition_name) {
        return Err(RuntimeError::Program(format!(
            "BASIC64 module definition PROC {definition_name} is missing"
        )));
    }
    for definition in program.procedures.values_mut() {
        definition.entry += 2;
    }
    for definition in program.functions.values_mut() {
        definition.entry += 2;
    }
    for address in program.line_entries.values_mut() {
        *address += 2;
    }
    program.instructions.insert(
        0,
        super::parser::LocatedStatement {
            line_number: 0,
            statement: Statement::ProcedureCall(definition_name, Vec::new()),
        },
    );
    program.instructions.insert(
        1,
        super::parser::LocatedStatement {
            line_number: 0,
            statement: Statement::End,
        },
    );

    let mut interpreter = Interpreter::new(program);
    let state = workspace
        .0
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    interpreter.variables.extend(
        state
            .iter()
            .map(|(name, value)| (name.clone(), value.clone())),
    );
    let initialized_readonly = interpreter
        .readonly_bindings
        .iter()
        .filter(|name| state.contains_key(*name))
        .cloned()
        .collect::<Vec<_>>();
    interpreter
        .readonly_initialized
        .extend(initialized_readonly);
    drop(state);
    for (name, kind) in persistent_state {
        if !interpreter.variables.contains_key(name) {
            let value = match kind {
                super::parser::SystemType::Handle(type_name) => Value::Handle {
                    type_name: type_name.clone(),
                    raw: 0,
                },
                _ => default_for_system_type(kind, &interpreter.program.system_types, 0, task.id)?,
            };
            interpreter.variables.insert(name.clone(), value);
        }
    }
    for (index, value) in context.registers.into_iter().enumerate() {
        let kind = contract
            .registers
            .iter()
            .find(|register| usize::from(register.register) == index)
            .map(|register| &register.kind);
        let value = match kind {
            Some(crate::ricochet::RegisterKind::OpaqueHandle { type_name }) => Value::Handle {
                type_name: type_name.to_ascii_uppercase(),
                raw: value,
            },
            Some(crate::ricochet::RegisterKind::LogicalAddress { .. }) => Value::LogicalAddress {
                owner_task: task.id,
                raw: value,
            },
            Some(crate::ricochet::RegisterKind::Signed { .. }) => {
                Value::Number(f64::from(value as i32))
            }
            _ => Value::Number(f64::from(value)),
        };
        interpreter.variables.insert(format!("R{index}%"), value);
    }
    interpreter.variables.insert(
        "PC%".into(),
        Value::LogicalAddress {
            owner_task: task.id,
            raw: context.pc,
        },
    );
    interpreter.variables.insert(
        "CARRY%".into(),
        Value::Number(f64::from(u8::from(context.carry))),
    );

    dispatcher.with_module_execution(module_id, |dispatcher| interpreter.run(task, dispatcher))?;

    let names = persistent_state.keys().cloned().collect::<Vec<_>>();
    let values = names
        .iter()
        .map(|name| {
            interpreter
                .variables
                .get(name)
                .cloned()
                .unwrap_or_else(|| default_value(name))
        })
        .collect::<Vec<_>>();
    validate_system_arguments(
        &persistent_state.values().cloned().collect::<Vec<_>>(),
        &values,
        0,
    )?;
    workspace
        .0
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .extend(names.into_iter().zip(values));

    for register in &contract.registers {
        if matches!(
            register.direction,
            crate::ricochet::ArgumentDirection::Out | crate::ricochet::ArgumentDirection::InOut
        ) {
            let value = interpreter.get_variable(&format!("R{}%", register.register));
            context.registers[usize::from(register.register)] =
                value_to_contract_register(&value, &register.kind, task.id, 0)?;
        }
    }
    if matches!(
        contract.program_counter,
        Some(crate::ricochet::ArgumentDirection::Out | crate::ricochet::ArgumentDirection::InOut)
    ) {
        context.pc = match interpreter.get_variable("PC%") {
            Value::LogicalAddress { owner_task, raw } if owner_task == task.id => raw,
            Value::LogicalAddress { .. } => {
                return Err(program_error(
                    0,
                    "program counter belongs to a different caller task",
                ));
            }
            Value::Number(number)
                if (0.0..=f64::from(u32::MAX)).contains(&number) && number.fract() == 0.0 =>
            {
                number as u32
            }
            _ => {
                return Err(program_error(
                    0,
                    "program counter must remain a checked 32-bit address",
                ));
            }
        };
    }
    if matches!(
        contract.carry,
        Some(crate::ricochet::ArgumentDirection::Out | crate::ricochet::ArgumentDirection::InOut)
    ) {
        context.carry = interpreter.get_variable("CARRY%").number(0)? != 0.0;
    }
    Ok(())
}

pub(crate) fn invoke_imported_system_symbol(
    mut program: ParsedProgram,
    name: &str,
    function: bool,
    values: Vec<Value>,
    module_id: crate::ricochet::ModuleId,
    workspace: &ModuleWorkspace,
    persistent_state: &std::collections::BTreeMap<String, super::parser::SystemType>,
    task: &mut Task,
    dispatcher: &mut SwiDispatcher,
) -> Result<Option<Value>, RuntimeError> {
    let name = name.to_ascii_uppercase();
    let parameters = if function {
        program.functions.get(&name)
    } else {
        program.procedures.get(&name)
    }
    .ok_or_else(|| program_error(0, format!("imported symbol {name} is not defined")))?
    .parameters
    .clone();
    if values.len() != parameters.len() {
        return Err(program_error(
            0,
            format!("imported symbol {name} argument count mismatch"),
        ));
    }
    if function && !program.typed_results.contains_key(&name) {
        return Err(program_error(
            0,
            format!("imported FN {name} has no declared result type"),
        ));
    }
    for definition in program.procedures.values_mut() {
        definition.entry += 2;
    }
    for definition in program.functions.values_mut() {
        definition.entry += 2;
    }
    for address in program.line_entries.values_mut() {
        *address += 2;
    }
    let argument_names = (0..values.len())
        .map(|index| format!("__RICOCHET_IMPORTED_ARGUMENT_{index}"))
        .collect::<Vec<_>>();
    let invocation = if function {
        let result_name = "__RICOCHET_IMPORTED_RESULT";
        let arguments = argument_names.iter().cloned().map(Expr::Variable).collect();
        Statement::Assign(
            LValue::Variable(result_name.into()),
            Expr::UserFunction(name, arguments),
        )
    } else {
        Statement::ProcedureCall(
            name,
            argument_names.iter().cloned().map(Expr::Variable).collect(),
        )
    };
    program.instructions.insert(
        0,
        super::parser::LocatedStatement {
            line_number: 0,
            statement: invocation,
        },
    );
    program.instructions.insert(
        1,
        super::parser::LocatedStatement {
            line_number: 0,
            statement: Statement::End,
        },
    );

    let mut interpreter = Interpreter::new(program);
    for (name, value) in argument_names.into_iter().zip(values) {
        interpreter.variables.insert(name, value);
    }
    let state = workspace
        .0
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    interpreter.variables.extend(
        state
            .iter()
            .map(|(name, value)| (name.clone(), value.clone())),
    );
    interpreter.readonly_initialized.extend(
        interpreter
            .readonly_bindings
            .iter()
            .filter(|name| state.contains_key(*name))
            .cloned(),
    );
    drop(state);
    for (name, kind) in persistent_state {
        if !interpreter.variables.contains_key(name) {
            let value = match kind {
                super::parser::SystemType::Handle(type_name) => Value::Handle {
                    type_name: type_name.clone(),
                    raw: 0,
                },
                _ => default_for_system_type(kind, &interpreter.program.system_types, 0, task.id)?,
            };
            interpreter.variables.insert(name.clone(), value);
        }
    }
    dispatcher.with_module_execution(module_id, |dispatcher| interpreter.run(task, dispatcher))?;
    let names = persistent_state.keys().cloned().collect::<Vec<_>>();
    let state_values = names
        .iter()
        .map(|name| {
            interpreter
                .variables
                .get(name)
                .cloned()
                .unwrap_or_else(|| default_value(name))
        })
        .collect::<Vec<_>>();
    validate_system_arguments(
        &persistent_state.values().cloned().collect::<Vec<_>>(),
        &state_values,
        0,
    )?;
    workspace
        .0
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .extend(names.into_iter().zip(state_values));
    Ok(function.then(|| {
        interpreter
            .variables
            .get("__RICOCHET_IMPORTED_RESULT")
            .cloned()
            .unwrap_or_else(|| default_value("__RICOCHET_IMPORTED_RESULT"))
    }))
}

fn validate_system_arguments(
    types: &[super::parser::SystemType],
    values: &[Value],
    line: u16,
) -> Result<(), RuntimeError> {
    if types.len() != values.len() {
        return Err(program_error(
            line,
            "typed procedure parameter count mismatch",
        ));
    }
    for (kind, value) in types.iter().zip(values) {
        match (kind, value) {
            (super::parser::SystemType::String, Value::String(_)) => {}
            (super::parser::SystemType::Record(expected), Value::Record { type_name, .. })
                if expected == type_name => {}
            (super::parser::SystemType::Enum(expected), Value::Enum { type_name, .. })
                if expected == type_name => {}
            (super::parser::SystemType::Flags(expected), Value::Flags { type_name, .. })
                if expected == type_name => {}
            (super::parser::SystemType::Handle(expected), Value::Handle { type_name, .. })
                if expected == type_name => {}
            (super::parser::SystemType::Error(expected), Value::Error { type_name, .. })
                if expected == type_name => {}
            (super::parser::SystemType::Byte, Value::Number(number))
                if (0.0..=255.0).contains(number) && number.fract() == 0.0 => {}
            (super::parser::SystemType::UInt16, Value::Number(number))
                if (0.0..=f64::from(u16::MAX)).contains(number) && number.fract() == 0.0 => {}
            (super::parser::SystemType::UInt32, Value::Number(number))
                if (0.0..=f64::from(u32::MAX)).contains(number) && number.fract() == 0.0 => {}
            (super::parser::SystemType::Int32, Value::Number(number))
                if (f64::from(i32::MIN)..=f64::from(i32::MAX)).contains(number)
                    && number.fract() == 0.0 => {}
            (super::parser::SystemType::Byte, Value::Integer(number))
                if (0..=i128::from(u8::MAX)).contains(number) => {}
            (super::parser::SystemType::UInt16, Value::Integer(number))
                if (0..=i128::from(u16::MAX)).contains(number) => {}
            (super::parser::SystemType::UInt32, Value::Integer(number))
                if (0..=i128::from(u32::MAX)).contains(number) => {}
            (super::parser::SystemType::Int32, Value::Integer(number))
                if (i128::from(i32::MIN)..=i128::from(i32::MAX)).contains(number) => {}
            (super::parser::SystemType::UInt64, Value::Integer(number))
                if (0..=i128::from(u64::MAX)).contains(number) => {}
            (super::parser::SystemType::Int64, Value::Integer(number))
                if (i128::from(i64::MIN)..=i128::from(i64::MAX)).contains(number) => {}
            (super::parser::SystemType::UInt64, Value::UInt64(_)) => {}
            (super::parser::SystemType::Int64, Value::Int64(_)) => {}
            (super::parser::SystemType::UInt64, Value::Number(number))
                if (0.0..=9_007_199_254_740_992.0).contains(number) && number.fract() == 0.0 => {}
            (super::parser::SystemType::Int64, Value::Number(number))
                if (-9_007_199_254_740_992.0..=9_007_199_254_740_992.0).contains(number)
                    && number.fract() == 0.0 => {}
            (super::parser::SystemType::UInt64, Value::Number(number))
                if (0.0..=9_007_199_254_740_992.0).contains(number) && number.fract() == 0.0 => {}
            (super::parser::SystemType::Int64, Value::Number(number))
                if (-9_007_199_254_740_992.0..=9_007_199_254_740_992.0).contains(number)
                    && number.fract() == 0.0 => {}
            (super::parser::SystemType::Address32, Value::Number(number))
                if (0.0..=f64::from(u32::MAX)).contains(number) && number.fract() == 0.0 => {}
            (super::parser::SystemType::Address32, Value::LogicalAddress { .. }) => {}
            _ => {
                return Err(program_error(
                    line,
                    format!("argument does not satisfy declared System Profile type {kind:?}"),
                ));
            }
        }
    }
    Ok(())
}

fn normalize_system_arguments(
    types: &[super::parser::SystemType],
    values: Vec<Value>,
    line: u16,
    task_id: u64,
) -> Result<Vec<Value>, RuntimeError> {
    if types.len() != values.len() {
        return Err(program_error(
            line,
            "typed procedure parameter count mismatch",
        ));
    }
    types
        .iter()
        .zip(values)
        .map(|(kind, value)| match (kind, value) {
            (super::parser::SystemType::UInt64, Value::Integer(number))
                if (0..=i128::from(u64::MAX)).contains(&number) =>
            {
                Ok(Value::UInt64(number as u64))
            }
            (super::parser::SystemType::Int64, Value::Integer(number))
                if (i128::from(i64::MIN)..=i128::from(i64::MAX)).contains(&number) =>
            {
                Ok(Value::Int64(number as i64))
            }
            (super::parser::SystemType::UInt64, value @ Value::UInt64(_))
            | (super::parser::SystemType::Int64, value @ Value::Int64(_)) => Ok(value),
            (super::parser::SystemType::UInt64, Value::Number(number))
                if (0.0..=9_007_199_254_740_992.0).contains(&number) && number.fract() == 0.0 =>
            {
                Ok(Value::UInt64(number as u64))
            }
            (super::parser::SystemType::Int64, Value::Number(number))
                if (-9_007_199_254_740_992.0..=9_007_199_254_740_992.0).contains(&number)
                    && number.fract() == 0.0 =>
            {
                Ok(Value::Int64(number as i64))
            }
            (super::parser::SystemType::Byte, Value::Integer(number))
                if (0..=i128::from(u8::MAX)).contains(&number) =>
            {
                Ok(Value::Number(number as f64))
            }
            (super::parser::SystemType::UInt16, Value::Integer(number))
                if (0..=i128::from(u16::MAX)).contains(&number) =>
            {
                Ok(Value::Number(number as f64))
            }
            (super::parser::SystemType::UInt32, Value::Integer(number))
                if (0..=i128::from(u32::MAX)).contains(&number) =>
            {
                Ok(Value::Number(number as f64))
            }
            (super::parser::SystemType::Int32, Value::Integer(number))
                if (i128::from(i32::MIN)..=i128::from(i32::MAX)).contains(&number) =>
            {
                Ok(Value::Number(number as f64))
            }
            (super::parser::SystemType::Address32, Value::Number(number))
                if (0.0..=f64::from(u32::MAX)).contains(&number) && number.fract() == 0.0 =>
            {
                Ok(Value::LogicalAddress {
                    owner_task: task_id,
                    raw: number as u32,
                })
            }
            (super::parser::SystemType::Address32, Value::LogicalAddress { owner_task, raw })
                if owner_task == task_id =>
            {
                Ok(Value::LogicalAddress { owner_task, raw })
            }
            (super::parser::SystemType::Address32, Value::LogicalAddress { .. }) => Err(
                program_error(line, "logical address belongs to a different caller task"),
            ),
            (kind, value) => {
                validate_system_arguments(
                    std::slice::from_ref(kind),
                    std::slice::from_ref(&value),
                    line,
                )?;
                Ok(value)
            }
        })
        .collect()
}

fn value_to_primitive_register(
    value: &Value,
    kind: &crate::ricochet::RegisterKind,
    task_id: u64,
    line: u16,
) -> Result<u32, RuntimeError> {
    match kind {
        crate::ricochet::RegisterKind::OpaqueHandle { type_name } => match value {
            Value::Handle {
                type_name: actual,
                raw,
            } if actual.eq_ignore_ascii_case(type_name) => Ok(*raw),
            _ => Err(program_error(
                line,
                format!("primitive requires opaque handle {type_name}"),
            )),
        },
        crate::ricochet::RegisterKind::Unsigned { bits } if *bits < 32 => {
            let number = value.number(line)?;
            if !(0.0..(1_u32 << bits) as f64).contains(&number) || number.fract() != 0.0 {
                return Err(program_error(
                    line,
                    format!("primitive argument violates U{bits}"),
                ));
            }
            Ok(number as u32)
        }
        crate::ricochet::RegisterKind::Unsigned { .. } => {
            let number = value.number(line)?;
            if !(0.0..=f64::from(u32::MAX)).contains(&number) || number.fract() != 0.0 {
                return Err(program_error(line, "primitive argument is not a UINT32"));
            }
            Ok(number as u32)
        }
        crate::ricochet::RegisterKind::LogicalAddress { bits: 32 } => match value {
            Value::LogicalAddress { owner_task, raw } if *owner_task == task_id => Ok(*raw),
            Value::LogicalAddress { .. } => Err(program_error(
                line,
                "logical address belongs to a different caller task",
            )),
            _ => Err(program_error(
                line,
                "primitive requires a caller-scoped ADDRESS32",
            )),
        },
        crate::ricochet::RegisterKind::LogicalAddress { bits } => match value {
            Value::LogicalAddress { owner_task, raw }
                if *owner_task == task_id && u64::from(*raw) < (1_u64 << bits) =>
            {
                Ok(*raw)
            }
            Value::LogicalAddress { owner_task, .. } if *owner_task != task_id => Err(
                program_error(line, "logical address belongs to a different caller task"),
            ),
            _ => Err(program_error(
                line,
                format!("primitive requires ADDRESS{bits}"),
            )),
        },
        crate::ricochet::RegisterKind::Signed { bits } if *bits < 32 => {
            let number = value.number(line)?;
            let bound = 2_f64.powi(i32::from(*bits) - 1);
            if !(-bound..bound).contains(&number) || number.fract() != 0.0 {
                return Err(program_error(
                    line,
                    format!("primitive argument violates S{bits}"),
                ));
            }
            Ok(number as i32 as u32)
        }
        crate::ricochet::RegisterKind::Signed { .. } => {
            let number = value.number(line)?;
            if !(f64::from(i32::MIN)..=f64::from(i32::MAX)).contains(&number)
                || number.fract() != 0.0
            {
                return Err(program_error(line, "primitive argument is not an INT32"));
            }
            Ok(number as i32 as u32)
        }
    }
}

fn value_to_sys_register(value: &Value, line: u16, task_id: u64) -> Result<u32, RuntimeError> {
    match value {
        Value::LogicalAddress { owner_task, raw } if *owner_task == task_id => Ok(*raw),
        Value::LogicalAddress { .. } => Err(program_error(
            line,
            "logical address belongs to a different caller task",
        )),
        value => Ok(value.number(line)?.trunc() as i32 as u32),
    }
}

fn value_to_contract_register(
    value: &Value,
    kind: &crate::ricochet::RegisterKind,
    task_id: u64,
    line: u16,
) -> Result<u32, RuntimeError> {
    if matches!(kind, crate::ricochet::RegisterKind::Unsigned { bits: 32 }) {
        match value {
            Value::Number(number)
                if (f64::from(i32::MIN)..0.0).contains(number) && number.fract() == 0.0 =>
            {
                return Ok(*number as i32 as u32);
            }
            Value::Integer(number) if (i128::from(i32::MIN)..0).contains(number) => {
                return Ok(*number as i32 as u32);
            }
            Value::Int64(number) if (i64::from(i32::MIN)..0).contains(number) => {
                return Ok(*number as i32 as u32);
            }
            _ => {}
        }
    }
    value_to_primitive_register(value, kind, task_id, line)
}

fn default_for_system_type(
    kind: &super::parser::SystemType,
    definitions: &std::collections::BTreeMap<String, super::parser::SystemTypeDefinition>,
    line: u16,
    task_id: u64,
) -> Result<Value, RuntimeError> {
    use super::parser::{SystemType, SystemTypeDefinition};
    Ok(match kind {
        SystemType::String => Value::String(Vec::new()),
        SystemType::Enum(type_name) => {
            let Some(SystemTypeDefinition::Enum { members, .. }) = definitions.get(type_name)
            else {
                return Err(program_error(
                    line,
                    format!("enum type {type_name} is missing"),
                ));
            };
            let value = members
                .values()
                .copied()
                .find(|value| *value == 0)
                .or_else(|| members.values().next().copied())
                .unwrap_or(0);
            Value::Enum {
                type_name: type_name.clone(),
                value,
            }
        }
        SystemType::Flags(type_name) => Value::Flags {
            type_name: type_name.clone(),
            value: 0,
        },
        SystemType::Record(type_name) => {
            let Some(SystemTypeDefinition::Record { fields: schema }) = definitions.get(type_name)
            else {
                return Err(program_error(
                    line,
                    format!("record type {type_name} is missing"),
                ));
            };
            let mut fields = HashMap::new();
            let mut readonly_fields = std::collections::HashSet::new();
            for field in schema {
                fields.insert(
                    field.name.clone(),
                    default_for_system_type(&field.value_type, definitions, line, task_id)?,
                );
                if field.read_only {
                    readonly_fields.insert(field.name.clone());
                }
            }
            Value::Record {
                type_name: type_name.clone(),
                fields,
                readonly_fields,
            }
        }
        SystemType::Handle(type_name) => Value::Handle {
            type_name: type_name.clone(),
            raw: 0,
        },
        SystemType::Error(type_name) => {
            let Some(SystemTypeDefinition::Error { fields: schema }) = definitions.get(type_name)
            else {
                return Err(program_error(
                    line,
                    format!("error type {type_name} is missing"),
                ));
            };
            let mut fields = HashMap::new();
            let mut readonly_fields = std::collections::HashSet::new();
            for field in schema {
                let value = match field.name.as_str() {
                    "CODE" => Value::Number(0.0),
                    "MESSAGE" => Value::String(Vec::new()),
                    _ => default_for_system_type(&field.value_type, definitions, line, task_id)?,
                };
                fields.insert(field.name.clone(), value);
                if field.read_only {
                    readonly_fields.insert(field.name.clone());
                }
            }
            Value::Error {
                type_name: type_name.clone(),
                code: 0,
                message: Vec::new(),
                fields,
                readonly_fields,
            }
        }
        SystemType::Byte | SystemType::UInt16 | SystemType::UInt32 | SystemType::Int32 => {
            Value::Number(0.0)
        }
        SystemType::UInt64 => Value::UInt64(0),
        SystemType::Int64 => Value::Int64(0),
        SystemType::Address32 => Value::LogicalAddress {
            owner_task: task_id,
            raw: 0,
        },
    })
}

fn set_nested_record_field(
    record: &mut Value,
    path: &[String],
    value: Value,
    line: u16,
) -> Result<(), RuntimeError> {
    let (fields, readonly_fields) = match record {
        Value::Record {
            fields,
            readonly_fields,
            ..
        }
        | Value::Error {
            fields,
            readonly_fields,
            ..
        } => (fields, readonly_fields),
        _ => {
            return Err(program_error(
                line,
                "nested assignment target is not a record",
            ));
        }
    };
    if path.len() == 1 {
        if readonly_fields.contains(&path[0]) {
            return Err(program_error(
                line,
                format!("{}.{} is read-only", record_type_name(record), path[0]),
            ));
        }
        fields.insert(path[0].clone(), value);
        return Ok(());
    }
    let next = fields
        .get_mut(&path[0])
        .ok_or_else(|| program_error(line, format!("unknown field {}", path[0])))?;
    set_nested_record_field(next, &path[1..], value, line)
}

fn record_type_name(value: &Value) -> &str {
    match value {
        Value::Record { type_name, .. } | Value::Error { type_name, .. } => type_name,
        _ => "<non-record>",
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

fn match_try_regions(program: &ParsedProgram) -> HashMap<usize, TryRegion> {
    let mut stack: Vec<(usize, Option<(usize, String, String)>)> = Vec::new();
    let mut regions = HashMap::new();
    for (address, instruction) in program.instructions.iter().enumerate() {
        match &instruction.statement {
            Statement::Try => stack.push((address, None)),
            Statement::Catch {
                error_name,
                error_type,
            } => {
                if let Some((_, catch)) = stack.last_mut() {
                    *catch = Some((address, error_name.clone(), error_type.clone()));
                }
            }
            Statement::EndTry => {
                if let Some((start, Some((catch_address, error_name, error_type)))) = stack.pop() {
                    regions.insert(
                        start,
                        TryRegion {
                            catch_address,
                            end_address: address,
                            error_name,
                            error_type,
                        },
                    );
                }
            }
            _ => {}
        }
    }
    regions
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
            Value::Number(_)
            | Value::Integer(_)
            | Value::UInt64(_)
            | Value::Int64(_)
            | Value::Enum { .. }
            | Value::Flags { .. }
            | Value::Record { .. }
            | Value::Handle { .. }
            | Value::LogicalAddress { .. }
            | Value::Error { .. } => Err(program_error(
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

fn integral_address_offset(number: f64, line: u16) -> Result<i64, RuntimeError> {
    if !number.is_finite()
        || number.fract() != 0.0
        || !(-f64::from(u32::MAX)..=f64::from(u32::MAX)).contains(&number)
    {
        return Err(program_error(
            line,
            "ADDRESS32 arithmetic requires an integral 32-bit offset",
        ));
    }
    Ok(number as i64)
}

fn address_offset_value(value: &Value, line: u16) -> Result<Option<i64>, RuntimeError> {
    match value {
        Value::Number(number) => integral_address_offset(*number, line).map(Some),
        Value::Integer(number) => i64::try_from(*number)
            .map(Some)
            .map_err(|_| program_error(line, "logical address offset exceeds signed 64-bit range")),
        Value::Int64(number) => Ok(Some(*number)),
        Value::UInt64(number) => i64::try_from(*number)
            .map(Some)
            .map_err(|_| program_error(line, "logical address offset exceeds signed 64-bit range")),
        _ => Ok(None),
    }
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

#[cfg(test)]
mod tests {
    use super::{Interpreter, Value, value_to_sys_register};

    use crate::{host::HostConsole, memory::Task, swi::SwiDispatcher};

    #[test]
    fn sys_register_accepts_only_same_task_logical_addresses() {
        let address = Value::LogicalAddress {
            owner_task: 910,
            raw: 0x1234,
        };
        assert_eq!(value_to_sys_register(&address, 1, 910).unwrap(), 0x1234);
        assert!(matches!(
            value_to_sys_register(&address, 1, 911),
            Err(crate::error::RuntimeError::Program(message))
                if message.contains("different caller task")
        ));
        assert_eq!(
            value_to_sys_register(&Value::Number(17.0), 1, 910).unwrap(),
            17
        );
    }

    #[test]
    fn classic_and_hybrid_integer_literals_keep_the_floating_number_model() {
        for mode in ["CLASSIC", "HYBRID"] {
            let source = format!("REM @BASIC64 MODE={mode}\nVALUE = 9007199254740993\n");
            let parsed = crate::basic_compat::parser::parse_source(&source).unwrap();
            let mut interpreter = Interpreter::new(parsed);
            let mut dispatcher = SwiDispatcher::new(HostConsole::stdio());
            interpreter
                .run(&mut Task::new(910), &mut dispatcher)
                .unwrap();

            assert!(matches!(
                interpreter.get_variable("VALUE"),
                Value::Number(value) if value == 9_007_199_254_740_992.0
            ));
        }
    }

    #[test]
    fn inline_if_resumes_after_procedure_call_and_honours_endproc() {
        let source = "10 A%=0:IF 1 THEN PROC BUMP:A%=A%+1\n20 PROC EARLY\n30 PRINT A%\n40 END\nDEF PROC BUMP\nA%=A%+10\nENDPROC\nDEF PROC EARLY\nIF 1 THEN ENDPROC:A%=99\nA%=A%+100\nENDPROC\n";
        let parsed = crate::basic_compat::parser::parse_source(source).unwrap();
        let mut interpreter = Interpreter::new(parsed);
        let mut dispatcher = SwiDispatcher::new(HostConsole::stdio());
        interpreter
            .run(&mut Task::new(910), &mut dispatcher)
            .unwrap();

        assert!(matches!(
            interpreter.get_variable("A%"),
            Value::Number(value) if value == 11.0
        ));

        let nested_source = "10 A%=0\n20 IF 1 THEN IF 1 THEN PROC BUMP:A%=A%*10+1:A%=A%*10+2\n30 PRINT A%\n40 END\nDEF PROC BUMP\nA%=A%*10+3\nENDPROC\n";
        let parsed = crate::basic_compat::parser::parse_source(nested_source).unwrap();
        let mut interpreter = Interpreter::new(parsed);
        let mut dispatcher = SwiDispatcher::new(HostConsole::stdio());
        interpreter
            .run(&mut Task::new(910), &mut dispatcher)
            .unwrap();
        assert!(matches!(
            interpreter.get_variable("A%"),
            Value::Number(value) if value == 312.0
        ));
    }
}
