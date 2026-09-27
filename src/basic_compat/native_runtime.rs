//! Checked services used by strict, whole-program BASIC native code.
//!
//! These entry points implement runtime services and individual dynamic-value
//! operations. They never receive a BASIC AST and never dispatch a BASIC
//! statement or recursively evaluate an expression.

use std::{
    collections::HashMap,
    ffi::c_void,
    panic::{AssertUnwindSafe, catch_unwind},
};

use crate::{
    error::RuntimeError,
    memory::{GUEST_MEMORY_BASE, GUEST_MEMORY_SIZE, Task},
    swi::{MosClock, OS_CLI, OS_NEW_LINE, OS_READ_LINE, SwiContext, SwiDispatcher},
};

const FIRST_HEAP_ADDRESS: u32 = GUEST_MEMORY_BASE + 0x8000;
const INPUT_BUFFER: u32 = GUEST_MEMORY_BASE + 0x3000;
const INPUT_BUFFER_SIZE: u32 = 4096;
const CLI_STRING_BUFFER: u32 = GUEST_MEMORY_BASE + 0x6000;
const PRINT_ZONE_WIDTH: usize = 14;
const DEFAULT_PRINT_FORMAT: u32 = 0x0000_090A;
const MAX_EXECUTION_STEPS: u64 = 100_000_000_000;
const MAX_NATIVE_CALL_DEPTH: u64 = 256;

#[derive(Clone, Debug)]
pub(super) enum NativeValue {
    Number(f64),
    String(Vec<u8>),
}

#[derive(Clone, Debug, Default)]
pub(super) struct NativeProgramLayout {
    pub numeric_variables: Vec<String>,
    pub string_variables: Vec<String>,
    pub arrays: Vec<String>,
    pub constant_strings: Vec<Vec<u8>>,
    pub temporary_strings: usize,
    pub data: Vec<(u16, NativeValue)>,
}

/// State shared with a synchronous Cranelift program. The native ABI sees only
/// this opaque context and the stable numeric-slot buffer; guest addresses are
/// always translated through `Task::memory` by the checked helpers below.
pub(super) struct NativeExecutionContext {
    numeric_names: Vec<String>,
    numeric_indices: HashMap<String, usize>,
    pub(super) numeric_slots: Box<[f64]>,
    string_names: Vec<String>,
    string_values: Vec<Vec<u8>>,
    string_variable_count: usize,
    arrays: Vec<Option<Vec<NativeValue>>>,
    array_names: Vec<String>,
    data: Vec<(u16, NativeValue)>,
    data_cursor: usize,
    scopes: Vec<(bool, usize, NativeValue)>,
    task: *mut Task,
    dispatcher: *mut SwiDispatcher,
    clock: MosClock,
    pending_key: Option<u8>,
    print_column: usize,
    print_format: u32,
    next_heap_address: u32,
    random_state: u32,
    last_random_fraction: f64,
    steps: u64,
    call_depth: u64,
    helper_calls: u64,
    compiled_calls: u64,
    error: Option<RuntimeError>,
}

impl NativeExecutionContext {
    pub(super) fn new(
        layout: NativeProgramLayout,
        task: &mut Task,
        dispatcher: &mut SwiDispatcher,
    ) -> Self {
        let numeric_names = layout.numeric_variables;
        let numeric_indices = numeric_names
            .iter()
            .enumerate()
            .map(|(index, name)| (name.clone(), index))
            .collect();
        let string_names = layout.string_variables;
        let string_variable_count = string_names.len();
        let mut string_values = vec![Vec::new(); string_variable_count];
        string_values.extend(layout.constant_strings);
        string_values.resize(
            string_values.len().saturating_add(layout.temporary_strings),
            Vec::new(),
        );
        let array_names = layout.arrays;
        Self {
            numeric_slots: vec![0.0; numeric_names.len()].into_boxed_slice(),
            numeric_names,
            numeric_indices,
            string_names,
            string_values,
            string_variable_count,
            arrays: vec![None; array_names.len()],
            array_names,
            data: layout.data,
            data_cursor: 0,
            scopes: Vec::new(),
            task,
            dispatcher,
            clock: dispatcher.system_clock(),
            pending_key: None,
            print_column: 0,
            print_format: DEFAULT_PRINT_FORMAT,
            next_heap_address: FIRST_HEAP_ADDRESS,
            random_state: 0xA341_316C,
            last_random_fraction: 0.0,
            steps: 0,
            call_depth: 0,
            helper_calls: 0,
            compiled_calls: 0,
            error: None,
        }
    }

    pub(super) fn as_opaque(&mut self) -> *mut c_void {
        (self as *mut Self).cast()
    }

    pub(super) fn numeric_slots_ptr(&mut self) -> *mut f64 {
        self.numeric_slots.as_mut_ptr()
    }

    pub(super) fn take_error(&mut self) -> Option<RuntimeError> {
        self.error.take()
    }

    pub(super) fn helper_calls(&self) -> u64 {
        self.helper_calls
    }

    pub(super) fn compiled_calls(&self) -> u64 {
        self.compiled_calls
    }

    fn array_mut(&mut self, index: i32, line: u16) -> Result<&mut Vec<NativeValue>, RuntimeError> {
        if index < 0 {
            return Err(program_error(line, "compiled array slot is invalid"));
        }
        let slot = self
            .arrays
            .get_mut(index as usize)
            .ok_or_else(|| program_error(line, "compiled array slot is invalid"))?;
        slot.as_mut()
            .ok_or_else(|| program_error(line, "array was not dimensioned"))
    }

    fn string(&self, index: i32, line: u16) -> Result<&[u8], RuntimeError> {
        if index < 0 {
            return Err(program_error(line, "compiled string slot is invalid"));
        }
        self.string_values
            .get(index as usize)
            .map(Vec::as_slice)
            .ok_or_else(|| program_error(line, "compiled string slot is invalid"))
    }

    fn string_mut(&mut self, index: i32, line: u16) -> Result<&mut Vec<u8>, RuntimeError> {
        if index < 0 {
            return Err(program_error(line, "compiled string slot is invalid"));
        }
        self.string_values
            .get_mut(index as usize)
            .ok_or_else(|| program_error(line, "compiled string slot is invalid"))
    }

    fn tick(&mut self, line: u16) -> Result<(), RuntimeError> {
        self.steps = self.steps.saturating_add(1);
        if self.steps > MAX_EXECUTION_STEPS {
            return Err(program_error(line, "execution step limit reached"));
        }
        if self.steps & 0x3FF == 0 && self.pending_key.is_none() {
            // SAFETY: these pointers are installed from the exclusively
            // borrowed task and dispatcher and remain valid for the run.
            if let Some(key) = unsafe { (&mut *self.dispatcher).poll_key() } {
                self.pending_key = Some(key);
            }
        }
        Ok(())
    }
}

fn line_number(line: i32) -> u16 {
    u16::try_from(line).unwrap_or_default()
}

fn program_error(line: u16, message: impl AsRef<str>) -> RuntimeError {
    RuntimeError::Program(format!("line {line}: {}", message.as_ref()))
}

fn as_runtime_error(error: RuntimeError, line: u16) -> RuntimeError {
    match error {
        RuntimeError::Program(message) if message.starts_with("line ") => {
            RuntimeError::Program(message)
        }
        error => program_error(line, error.to_string()),
    }
}

fn with_context<T: Copy>(
    pointer: *mut c_void,
    line: i32,
    fallback: T,
    operation: impl FnOnce(&mut NativeExecutionContext, u16) -> Result<T, RuntimeError>,
) -> T {
    if pointer.is_null() {
        return fallback;
    }
    // SAFETY: strict JIT calls are synchronous and retain the unique mutable
    // context borrow until the generated function returns.
    let context = unsafe { &mut *pointer.cast::<NativeExecutionContext>() };
    context.helper_calls = context.helper_calls.saturating_add(1);
    if context.error.is_some() {
        return fallback;
    }
    let line = line_number(line);
    match catch_unwind(AssertUnwindSafe(|| operation(context, line))) {
        Ok(Ok(value)) => value,
        Ok(Err(error)) => {
            context.error = Some(as_runtime_error(error, line));
            fallback
        }
        Err(_) => {
            context.error = Some(program_error(line, "native runtime helper panicked"));
            fallback
        }
    }
}

#[unsafe(no_mangle)]
pub(super) extern "C" fn strict_native_context_ok(pointer: *mut c_void) -> i32 {
    with_context(pointer, 0, 0, |context, _| {
        Ok(i32::from(context.error.is_none()))
    })
}

#[unsafe(no_mangle)]
pub(super) extern "C" fn strict_native_enter(pointer: *mut c_void, line: i32) -> i32 {
    with_context(pointer, line, 0, |context, line| {
        context.call_depth = context.call_depth.saturating_add(1);
        context.compiled_calls = context.compiled_calls.saturating_add(1);
        context.tick(line)?;
        if context.call_depth > MAX_NATIVE_CALL_DEPTH {
            return Err(program_error(line, "native call depth limit reached"));
        }
        Ok(1)
    })
}

#[unsafe(no_mangle)]
pub(super) extern "C" fn strict_native_exit(pointer: *mut c_void, line: i32) -> i32 {
    with_context(pointer, line, 0, |context, _| {
        context.call_depth = context.call_depth.saturating_sub(1);
        Ok(i32::from(context.error.is_none()))
    })
}

#[unsafe(no_mangle)]
pub(super) extern "C" fn strict_native_tick(pointer: *mut c_void, line: i32) -> i32 {
    with_context(pointer, line, 0, |context, line| {
        context.tick(line)?;
        Ok(1)
    })
}

#[unsafe(no_mangle)]
pub(super) extern "C" fn strict_native_fail(pointer: *mut c_void, reason: i32, line: i32) -> i32 {
    with_context(pointer, line, 0, |_context, line| {
        let message = match reason {
            0 => "division by zero",
            1 => "FOR STEP cannot be zero",
            2 => "RETURN or ENDPROC has no active call",
            3 => "function return was reached outside a function call",
            4 => "unsupported native operation reached",
            _ => "native runtime check failed",
        };
        Err(program_error(line, message))
    })
}

#[unsafe(no_mangle)]
pub(super) extern "C" fn strict_native_scope_begin(pointer: *mut c_void, line: i32) -> i32 {
    with_context(pointer, line, -1, |context, _| {
        i32::try_from(context.scopes.len())
            .map_err(|_| RuntimeError::Program("native local scope depth overflowed".into()))
    })
}

#[unsafe(no_mangle)]
pub(super) extern "C" fn strict_native_scope_save_number(
    pointer: *mut c_void,
    slot: i32,
    line: i32,
) -> i32 {
    with_context(pointer, line, 0, |context, line| {
        let slot = usize::try_from(slot)
            .map_err(|_| program_error(line, "compiled numeric slot is invalid"))?;
        let value = *context
            .numeric_slots
            .get(slot)
            .ok_or_else(|| program_error(line, "compiled numeric slot is invalid"))?;
        context
            .scopes
            .push((false, slot, NativeValue::Number(value)));
        Ok(1)
    })
}

#[unsafe(no_mangle)]
pub(super) extern "C" fn strict_native_scope_save_string(
    pointer: *mut c_void,
    slot: i32,
    line: i32,
) -> i32 {
    with_context(pointer, line, 0, |context, line| {
        let slot = usize::try_from(slot)
            .map_err(|_| program_error(line, "compiled string slot is invalid"))?;
        let value = context
            .string_values
            .get(slot)
            .cloned()
            .ok_or_else(|| program_error(line, "compiled string slot is invalid"))?;
        context
            .scopes
            .push((true, slot, NativeValue::String(value)));
        Ok(1)
    })
}

#[unsafe(no_mangle)]
pub(super) extern "C" fn strict_native_scope_restore(
    pointer: *mut c_void,
    mark: i32,
    line: i32,
) -> i32 {
    with_context(pointer, line, 0, |context, line| {
        let mark = usize::try_from(mark)
            .map_err(|_| program_error(line, "native local scope mark is invalid"))?;
        if mark > context.scopes.len() {
            return Err(program_error(line, "native local scope mark is invalid"));
        }
        while context.scopes.len() > mark {
            let (is_string, slot, value) = context.scopes.pop().expect("scope length checked");
            match (is_string, value) {
                (true, NativeValue::String(value)) => {
                    let target = context
                        .string_values
                        .get_mut(slot)
                        .ok_or_else(|| program_error(line, "compiled string slot is invalid"))?;
                    *target = value;
                }
                (false, NativeValue::Number(value)) => {
                    let target = context
                        .numeric_slots
                        .get_mut(slot)
                        .ok_or_else(|| program_error(line, "compiled numeric slot is invalid"))?;
                    *target = value;
                }
                _ => return Err(program_error(line, "native local scope type mismatch")),
            }
        }
        Ok(1)
    })
}

#[unsafe(no_mangle)]
pub(super) extern "C" fn strict_native_integer_operation(
    pointer: *mut c_void,
    operation: i32,
    left: f64,
    right: f64,
    line: i32,
) -> f64 {
    with_context(pointer, line, 0.0, |_context, line| {
        let left = left as i32;
        let right = right as i32;
        let value = match operation {
            0 => left.checked_div(right).ok_or_else(|| {
                program_error(
                    line,
                    if right == 0 {
                        "integer division by zero"
                    } else {
                        "integer division overflowed"
                    },
                )
            })?,
            1 => left.checked_rem(right).ok_or_else(|| {
                program_error(
                    line,
                    if right == 0 {
                        "MOD by zero"
                    } else {
                        "MOD overflowed"
                    },
                )
            })?,
            2 => left.wrapping_shl((right as u32) & 31),
            3 => left & right,
            4 => left | right,
            5 => !left,
            _ => return Err(program_error(line, "invalid native integer operation")),
        };
        Ok(f64::from(value))
    })
}

#[unsafe(no_mangle)]
pub(super) extern "C" fn strict_native_divide(
    pointer: *mut c_void,
    left: f64,
    right: f64,
    line: i32,
) -> f64 {
    with_context(pointer, line, 0.0, |_context, line| {
        if right == 0.0 {
            Err(program_error(line, "division by zero"))
        } else {
            Ok(left / right)
        }
    })
}

#[unsafe(no_mangle)]
pub(super) extern "C" fn strict_native_pow(
    pointer: *mut c_void,
    left: f64,
    right: f64,
    line: i32,
) -> f64 {
    with_context(pointer, line, 0.0, |_context, _| Ok(left.powf(right)))
}

#[unsafe(no_mangle)]
pub(super) extern "C" fn strict_native_time(pointer: *mut c_void, line: i32) -> f64 {
    with_context(pointer, line, 0.0, |context, _| {
        Ok(f64::from(context.clock.read() as u32 as i32))
    })
}

#[unsafe(no_mangle)]
pub(super) extern "C" fn strict_native_inkey(
    pointer: *mut c_void,
    has_argument: i32,
    line: i32,
) -> f64 {
    with_context(pointer, line, 0.0, |context, _| {
        let no_key = if has_argument == 0 { -256.0 } else { -1.0 };
        Ok(f64::from(
            context.pending_key.take().map_or(no_key as i32, i32::from),
        ))
    })
}

#[unsafe(no_mangle)]
pub(super) extern "C" fn strict_native_set_time(
    pointer: *mut c_void,
    value: f64,
    line: i32,
) -> i32 {
    with_context(pointer, line, 0, |context, _| {
        context.clock.set(value.trunc() as i32 as u32 as u64);
        Ok(1)
    })
}

#[unsafe(no_mangle)]
pub(super) extern "C" fn strict_native_set_string(
    pointer: *mut c_void,
    destination: i32,
    source: i32,
    line: i32,
) -> i32 {
    with_context(pointer, line, 0, |context, line| {
        let value = context.string(source, line)?.to_vec();
        let slot = usize::try_from(destination)
            .map_err(|_| program_error(line, "compiled string slot is invalid"))?;
        if slot < context.string_variable_count
            && context.string_names[slot].as_bytes().last() != Some(&b'$')
        {
            return Err(program_error(
                line,
                "string value assigned to a numeric variable",
            ));
        }
        *context.string_mut(destination, line)? = value;
        Ok(1)
    })
}

#[unsafe(no_mangle)]
pub(super) extern "C" fn strict_native_copy_string(
    pointer: *mut c_void,
    destination: i32,
    source: i32,
    line: i32,
) -> i32 {
    strict_native_set_string(pointer, destination, source, line)
}

#[unsafe(no_mangle)]
pub(super) extern "C" fn strict_native_string_concat(
    pointer: *mut c_void,
    left: i32,
    right: i32,
    destination: i32,
    line: i32,
) -> i32 {
    with_context(pointer, line, 0, |context, line| {
        let left = context.string(left, line)?.to_vec();
        let right = context.string(right, line)?;
        let mut joined = left;
        joined.extend_from_slice(right);
        *context.string_mut(destination, line)? = joined;
        Ok(1)
    })
}

#[unsafe(no_mangle)]
pub(super) extern "C" fn strict_native_string_compare(
    pointer: *mut c_void,
    left: i32,
    right: i32,
    operator: i32,
    line: i32,
) -> f64 {
    with_context(pointer, line, 0.0, |context, line| {
        let ordering = context
            .string(left, line)?
            .cmp(context.string(right, line)?);
        let result = match operator {
            0 => ordering.is_eq(),
            1 => !ordering.is_eq(),
            2 => ordering.is_lt(),
            3 => !ordering.is_gt(),
            4 => ordering.is_gt(),
            5 => !ordering.is_lt(),
            _ => return Err(program_error(line, "invalid native string comparison")),
        };
        Ok(if result { 1.0 } else { 0.0 })
    })
}

#[unsafe(no_mangle)]
pub(super) extern "C" fn strict_native_string_len(
    pointer: *mut c_void,
    source: i32,
    line: i32,
) -> f64 {
    with_context(pointer, line, 0.0, |context, line| {
        Ok(context.string(source, line)?.len() as f64)
    })
}

#[unsafe(no_mangle)]
pub(super) extern "C" fn strict_native_string_asc(
    pointer: *mut c_void,
    source: i32,
    line: i32,
) -> f64 {
    with_context(pointer, line, 0.0, |context, line| {
        Ok(context
            .string(source, line)?
            .first()
            .copied()
            .map_or(-1.0, f64::from))
    })
}

#[unsafe(no_mangle)]
pub(super) extern "C" fn strict_native_string_left(
    pointer: *mut c_void,
    source: i32,
    count: f64,
    destination: i32,
    line: i32,
) -> i32 {
    with_context(pointer, line, 0, |context, line| {
        let bytes = context.string(source, line)?.to_vec();
        let count = bounded_string_length(count, line)?;
        *context.string_mut(destination, line)? = bytes[..bytes.len().min(count)].to_vec();
        Ok(1)
    })
}

#[unsafe(no_mangle)]
pub(super) extern "C" fn strict_native_string_right(
    pointer: *mut c_void,
    source: i32,
    count: f64,
    destination: i32,
    line: i32,
) -> i32 {
    with_context(pointer, line, 0, |context, line| {
        let bytes = context.string(source, line)?.to_vec();
        let count = bounded_string_length(count, line)?;
        let start = bytes.len().saturating_sub(count);
        *context.string_mut(destination, line)? = bytes[start..].to_vec();
        Ok(1)
    })
}

#[unsafe(no_mangle)]
pub(super) extern "C" fn strict_native_string_mid(
    pointer: *mut c_void,
    source: i32,
    start: f64,
    length: f64,
    has_length: i32,
    destination: i32,
    line: i32,
) -> i32 {
    with_context(pointer, line, 0, |context, line| {
        let bytes = context.string(source, line)?.to_vec();
        let start = start.trunc().max(1.0) as usize - 1;
        let length = if has_length == 0 {
            bytes.len()
        } else {
            bounded_string_length(length, line)?
        };
        let start = start.min(bytes.len());
        let end = start.saturating_add(length).min(bytes.len());
        *context.string_mut(destination, line)? = bytes[start..end].to_vec();
        Ok(1)
    })
}

#[unsafe(no_mangle)]
pub(super) extern "C" fn strict_native_string_str(
    pointer: *mut c_void,
    value: f64,
    destination: i32,
    line: i32,
) -> i32 {
    with_context(pointer, line, 0, |context, line| {
        *context.string_mut(destination, line)? = format_number(value).into_bytes();
        Ok(1)
    })
}

#[unsafe(no_mangle)]
pub(super) extern "C" fn strict_native_string_chr(
    pointer: *mut c_void,
    value: f64,
    destination: i32,
    line: i32,
) -> i32 {
    with_context(pointer, line, 0, |context, line| {
        *context.string_mut(destination, line)? = vec![value.trunc() as i32 as u8];
        Ok(1)
    })
}

#[unsafe(no_mangle)]
pub(super) extern "C" fn strict_native_string_string(
    pointer: *mut c_void,
    count: f64,
    pattern: i32,
    destination: i32,
    line: i32,
) -> i32 {
    with_context(pointer, line, 0, |context, line| {
        let count = bounded_string_length(count, line)?;
        let byte = context
            .string(pattern, line)?
            .first()
            .copied()
            .unwrap_or_default();
        *context.string_mut(destination, line)? = vec![byte; count];
        Ok(1)
    })
}

#[unsafe(no_mangle)]
pub(super) extern "C" fn strict_native_string_val(
    pointer: *mut c_void,
    source: i32,
    line: i32,
) -> f64 {
    with_context(pointer, line, 0.0, |context, line| {
        Ok(parse_basic_val(context.string(source, line)?))
    })
}

#[unsafe(no_mangle)]
pub(super) extern "C" fn strict_native_string_instr(
    pointer: *mut c_void,
    text: i32,
    pattern: i32,
    start: f64,
    has_start: i32,
    line: i32,
) -> f64 {
    with_context(pointer, line, 0.0, |context, line| {
        let text = context.string(text, line)?;
        let pattern = context.string(pattern, line)?;
        let start = if has_start == 0 {
            0
        } else {
            bounded_string_length(start, line)?.saturating_sub(1)
        };
        let position = if start > text.len() {
            None
        } else {
            text[start..]
                .windows(pattern.len().max(1))
                .position(|window| !pattern.is_empty() && window == pattern)
                .map(|offset| start + offset + 1)
        };
        Ok(position.map_or(0.0, |index| index as f64))
    })
}

#[unsafe(no_mangle)]
pub(super) extern "C" fn strict_native_number_builtin(
    pointer: *mut c_void,
    token: i32,
    value: f64,
    line: i32,
) -> f64 {
    with_context(pointer, line, 0.0, |_context, line| match token as u8 {
        0x94 => Ok(value.abs()),
        0x9B => Ok(value.cos()),
        0xA8 => Ok(value.floor()),
        0xAA => Ok(value.ln()),
        0xAB => Ok(value.log10()),
        0xB5 => Ok(value.sin()),
        0xB6 => Ok(value.sqrt()),
        0xB7 => Ok(value.tan()),
        _ => Err(program_error(
            line,
            format!("unsupported numeric built-in &{:02X}", token as u8),
        )),
    })
}

#[unsafe(no_mangle)]
pub(super) extern "C" fn strict_native_random(
    pointer: *mut c_void,
    has_argument: i32,
    value: f64,
    line: i32,
) -> f64 {
    with_context(pointer, line, 0.0, |context, line| {
        let result = if has_argument == 0 {
            f64::from(next_random(context) as i32)
        } else {
            let integer = value.trunc() as i64;
            if value < 0.0 {
                let seed = integer as i32;
                context.random_state = if seed == 0 { 0xA341_316C } else { seed as u32 };
                f64::from(seed)
            } else {
                match integer {
                    0 => context.last_random_fraction,
                    1 => {
                        context.last_random_fraction =
                            f64::from(next_random(context)) / 4_294_967_296.0;
                        context.last_random_fraction
                    }
                    upper => {
                        let upper = u32::try_from(upper)
                            .map_err(|_| program_error(line, "RND limit is too large"))?;
                        if upper == 0 {
                            return Err(program_error(line, "RND limit cannot be zero"));
                        }
                        f64::from(next_random(context) % upper + 1)
                    }
                }
            }
        };
        Ok(result)
    })
}

fn next_random(context: &mut NativeExecutionContext) -> u32 {
    let mut value = context.random_state;
    value ^= value.wrapping_shl(13);
    value ^= value.wrapping_shr(17);
    value ^= value.wrapping_shl(5);
    context.random_state = value;
    value
}

#[unsafe(no_mangle)]
pub(super) extern "C" fn strict_native_array_dim(
    pointer: *mut c_void,
    array: i32,
    length: f64,
    byte_block: i32,
    line: i32,
) -> i32 {
    with_context(pointer, line, 0, |context, line| {
        if !length.is_finite() || length < 1.0 || length > 1_000_001.0 {
            return Err(program_error(
                line,
                "DIM size is outside the supported range",
            ));
        }
        let length = length.trunc() as usize;
        if array < 0 || array as usize >= context.arrays.len() {
            return Err(program_error(line, "compiled array slot is invalid"));
        }
        let name = context.array_names[array as usize].clone();
        if byte_block != 0 {
            let byte_count =
                u32::try_from(length).map_err(|_| program_error(line, "DIM block is too large"))?;
            let block_start = context
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
            // SAFETY: task is retained for this synchronous native invocation.
            unsafe {
                (&mut *context.task)
                    .memory
                    .write_bytes(context.next_heap_address, &vec![0; length])?
            };
            if let Some(slot) = context.numeric_indices.get(&name).copied() {
                context.numeric_slots[slot] = f64::from(block_start);
            }
            context.next_heap_address = block_end;
        } else {
            let initial = if name.ends_with('$') {
                NativeValue::String(Vec::new())
            } else {
                NativeValue::Number(0.0)
            };
            context.arrays[array as usize] = Some(vec![initial; length]);
        }
        Ok(1)
    })
}

fn checked_array_index(number: f64, line: u16) -> Result<usize, RuntimeError> {
    if !number.is_finite() || number < 0.0 || number > usize::MAX as f64 {
        return Err(program_error(
            line,
            "array index is outside the supported range",
        ));
    }
    Ok(number.trunc() as usize)
}

#[unsafe(no_mangle)]
pub(super) extern "C" fn strict_native_array_get_number(
    pointer: *mut c_void,
    array: i32,
    index: f64,
    line: i32,
) -> f64 {
    with_context(pointer, line, 0.0, |context, line| {
        let index = checked_array_index(index, line)?;
        match context.array_mut(array, line)?.get(index) {
            Some(NativeValue::Number(value)) => Ok(*value),
            Some(NativeValue::String(_)) => {
                Err(program_error(line, "expected a numeric array element"))
            }
            None => Err(program_error(
                line,
                format!("array index {index} is out of range"),
            )),
        }
    })
}

#[unsafe(no_mangle)]
pub(super) extern "C" fn strict_native_array_get_string(
    pointer: *mut c_void,
    array: i32,
    index: f64,
    destination: i32,
    line: i32,
) -> i32 {
    with_context(pointer, line, 0, |context, line| {
        let index = checked_array_index(index, line)?;
        let value = match context.array_mut(array, line)?.get(index) {
            Some(NativeValue::String(value)) => value.clone(),
            Some(NativeValue::Number(_)) => {
                return Err(program_error(line, "expected a string array element"));
            }
            None => {
                return Err(program_error(
                    line,
                    format!("array index {index} is out of range"),
                ));
            }
        };
        *context.string_mut(destination, line)? = value;
        Ok(1)
    })
}

#[unsafe(no_mangle)]
pub(super) extern "C" fn strict_native_array_set_number(
    pointer: *mut c_void,
    array: i32,
    index: f64,
    value: f64,
    line: i32,
) -> i32 {
    with_context(pointer, line, 0, |context, line| {
        let index = checked_array_index(index, line)?;
        let integer_array = context
            .array_names
            .get(
                usize::try_from(array)
                    .map_err(|_| program_error(line, "compiled array slot is invalid"))?,
            )
            .is_some_and(|name| name.ends_with('%'));
        let slot = context.array_mut(array, line)?;
        let target = slot
            .get_mut(index)
            .ok_or_else(|| program_error(line, format!("array index {index} is out of range")))?;
        match target {
            NativeValue::Number(target) => {
                *target = if integer_array {
                    (value.trunc() as i32) as f64
                } else {
                    value
                }
            }
            NativeValue::String(_) => {
                return Err(program_error(
                    line,
                    "numeric value assigned to a string array element",
                ));
            }
        }
        Ok(1)
    })
}

#[unsafe(no_mangle)]
pub(super) extern "C" fn strict_native_array_set_string(
    pointer: *mut c_void,
    array: i32,
    index: f64,
    value: i32,
    line: i32,
) -> i32 {
    with_context(pointer, line, 0, |context, line| {
        let index = checked_array_index(index, line)?;
        let value = context.string(value, line)?.to_vec();
        let name = context
            .array_names
            .get(array as usize)
            .ok_or_else(|| program_error(line, "compiled array slot is invalid"))?;
        if !name.ends_with('$') {
            return Err(program_error(
                line,
                "string value assigned to a numeric array element",
            ));
        }
        match context.array_mut(array, line)?.get_mut(index) {
            Some(NativeValue::String(target)) => *target = value,
            Some(NativeValue::Number(_)) => {
                return Err(program_error(
                    line,
                    "string value assigned to a numeric array element",
                ));
            }
            None => {
                return Err(program_error(
                    line,
                    format!("array index {index} is out of range"),
                ));
            }
        }
        Ok(1)
    })
}

#[unsafe(no_mangle)]
pub(super) extern "C" fn strict_native_data_read_number(pointer: *mut c_void, line: i32) -> f64 {
    with_context(pointer, line, 0.0, |context, line| {
        let Some((_, value)) = context.data.get(context.data_cursor).cloned() else {
            return Err(program_error(line, "READ passed the end of DATA"));
        };
        context.data_cursor += 1;
        match value {
            NativeValue::Number(value) => Ok(value),
            NativeValue::String(_) => Err(program_error(line, "READ value is not numeric")),
        }
    })
}

#[unsafe(no_mangle)]
pub(super) extern "C" fn strict_native_data_read_string(
    pointer: *mut c_void,
    destination: i32,
    line: i32,
) -> i32 {
    with_context(pointer, line, 0, |context, line| {
        let Some((_, value)) = context.data.get(context.data_cursor).cloned() else {
            return Err(program_error(line, "READ passed the end of DATA"));
        };
        context.data_cursor += 1;
        let NativeValue::String(value) = value else {
            return Err(program_error(line, "READ value is not a string"));
        };
        *context.string_mut(destination, line)? = value;
        Ok(1)
    })
}

#[unsafe(no_mangle)]
pub(super) extern "C" fn strict_native_data_restore(
    pointer: *mut c_void,
    cursor: i32,
    line: i32,
) -> i32 {
    with_context(pointer, line, 0, |context, line| {
        if cursor < 0 || cursor as usize > context.data.len() {
            return Err(program_error(
                line,
                "compiled DATA restore position is invalid",
            ));
        }
        context.data_cursor = cursor as usize;
        Ok(1)
    })
}

#[unsafe(no_mangle)]
pub(super) extern "C" fn strict_native_print_number(
    pointer: *mut c_void,
    value: f64,
    line: i32,
) -> i32 {
    with_context(pointer, line, 0, |context, _| {
        let bytes = format_print_number(value, context.print_format).into_bytes();
        emit(context, &bytes)?;
        Ok(1)
    })
}

#[unsafe(no_mangle)]
pub(super) extern "C" fn strict_native_print_string(
    pointer: *mut c_void,
    value: i32,
    line: i32,
) -> i32 {
    with_context(pointer, line, 0, |context, line| {
        let bytes = context.string(value, line)?.to_vec();
        emit(context, &bytes)?;
        Ok(1)
    })
}

#[unsafe(no_mangle)]
pub(super) extern "C" fn strict_native_print_spaces(
    pointer: *mut c_void,
    count: f64,
    line: i32,
) -> i32 {
    with_context(pointer, line, 0, |context, line| {
        let count = bounded_string_length(count, line)?;
        emit(context, &vec![b' '; count])?;
        Ok(1)
    })
}

#[unsafe(no_mangle)]
pub(super) extern "C" fn strict_native_print_comma(pointer: *mut c_void, line: i32) -> i32 {
    with_context(pointer, line, 0, |context, _| {
        let count = PRINT_ZONE_WIDTH - context.print_column % PRINT_ZONE_WIDTH;
        emit(context, &vec![b' '; count])?;
        Ok(1)
    })
}

#[unsafe(no_mangle)]
pub(super) extern "C" fn strict_native_print_newline(pointer: *mut c_void, line: i32) -> i32 {
    with_context(pointer, line, 0, |context, _| {
        // SAFETY: dispatcher is retained for this synchronous native run.
        unsafe {
            (&mut *context.dispatcher).dispatch(
                OS_NEW_LINE,
                &mut *context.task,
                &mut SwiContext::default(),
            )?;
        }
        context.print_column = 0;
        Ok(1)
    })
}

#[unsafe(no_mangle)]
pub(super) extern "C" fn strict_native_print_tab(
    pointer: *mut c_void,
    x: f64,
    y: f64,
    line: i32,
) -> i32 {
    with_context(pointer, line, 0, |context, _| {
        let x = x.trunc() as u8;
        let y = y.trunc() as u8;
        // SAFETY: dispatcher and task are retained for this synchronous run.
        unsafe {
            (&mut *context.dispatcher).write_via_os_write_c(&mut *context.task, &[31, x, y])?;
        }
        context.print_column = usize::from(x);
        Ok(1)
    })
}

#[unsafe(no_mangle)]
pub(super) extern "C" fn strict_native_print_format(
    pointer: *mut c_void,
    value: f64,
    line: i32,
) -> i32 {
    with_context(pointer, line, 0, |context, _| {
        let value = value.trunc() as i64;
        context.print_format = if value == 0 {
            DEFAULT_PRINT_FORMAT
        } else {
            value as u32
        };
        Ok(1)
    })
}

fn emit(context: &mut NativeExecutionContext, bytes: &[u8]) -> Result<(), RuntimeError> {
    if bytes.is_empty() {
        return Ok(());
    }
    // SAFETY: dispatcher and task are retained for this synchronous run.
    unsafe {
        (&mut *context.dispatcher).write_indirect(&mut *context.task, bytes)?;
    }
    for byte in bytes {
        if matches!(*byte, b'\n' | b'\r') {
            context.print_column = 0;
        } else {
            context.print_column = context.print_column.saturating_add(1);
        }
    }
    Ok(())
}

#[unsafe(no_mangle)]
pub(super) extern "C" fn strict_native_call(pointer: *mut c_void, address: f64, line: i32) -> i32 {
    with_context(pointer, line, 0, |context, line| {
        let address = checked_guest_address(address, line)?;
        let mut call = SwiContext::default();
        for (register, name) in ["A%", "X%", "Y%"].into_iter().enumerate() {
            let slot = context
                .numeric_indices
                .get(name)
                .copied()
                .unwrap_or(usize::MAX);
            let value = context.numeric_slots.get(slot).copied().unwrap_or(0.0) as i32;
            call.registers[register] = value as u32;
        }
        let carry_slot = context.numeric_indices.get("C%").copied();
        call.carry = carry_slot
            .and_then(|slot| context.numeric_slots.get(slot))
            .is_some_and(|value| (*value as i32 & 1) != 0);
        if address == 0xFFE0 || (address == 0xFFF4 && matches!(call.registers[0] & 255, 21 | 129)) {
            if let Some(key) = context.pending_key.take() {
                // SAFETY: dispatcher is retained for this synchronous run.
                unsafe {
                    (&mut *context.dispatcher).restore_polled_key(key);
                }
            }
        }
        // SAFETY: dispatcher and task are retained for this synchronous run.
        unsafe {
            (&mut *context.dispatcher).dispatch_mos_call(address, &mut *context.task, &mut call)?;
        }
        Ok(1)
    })
}

#[unsafe(no_mangle)]
pub(super) extern "C" fn strict_native_star(pointer: *mut c_void, command: i32, line: i32) -> i32 {
    with_context(pointer, line, 0, |context, line| {
        let bytes = context.string(command, line)?.to_vec();
        let terminator = CLI_STRING_BUFFER
            .checked_add(
                u32::try_from(bytes.len())
                    .map_err(|_| program_error(line, "CLI command is too long"))?,
            )
            .ok_or_else(|| program_error(line, "CLI command address overflowed"))?;
        // SAFETY: task and dispatcher remain valid for the synchronous call.
        unsafe {
            (&mut *context.task)
                .memory
                .write_bytes(CLI_STRING_BUFFER, &bytes)?;
            (&mut *context.task).memory.write_byte(terminator, 0)?;
            let mut call = SwiContext::default();
            call.registers[0] = CLI_STRING_BUFFER;
            (&mut *context.dispatcher).dispatch(OS_CLI, &mut *context.task, &mut call)?;
        }
        Ok(1)
    })
}

#[unsafe(no_mangle)]
pub(super) extern "C" fn strict_native_read_memory(
    pointer: *mut c_void,
    width: i32,
    address: f64,
    line: i32,
) -> f64 {
    with_context(pointer, line, 0.0, |context, line| {
        let address = checked_guest_address(address, line)?;
        // SAFETY: task is retained for this synchronous run.
        let task = unsafe { &*context.task };
        let value =
            match width {
                0 => u32::from(task.memory.read_byte(address)?),
                1 => {
                    let address = address;
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
                    u32::from_le_bytes(bytes)
                }
                _ => return Err(program_error(line, "invalid native memory width")),
            };
        Ok(f64::from(value as i32))
    })
}

#[unsafe(no_mangle)]
pub(super) extern "C" fn strict_native_write_memory(
    pointer: *mut c_void,
    width: i32,
    address: f64,
    value: f64,
    line: i32,
) -> i32 {
    with_context(pointer, line, 0, |context, line| {
        let address = checked_guest_address(address, line)?;
        let value = value as i32;
        // SAFETY: task is retained for this synchronous run.
        let task = unsafe { &mut *context.task };
        match width {
            0 => task.memory.write_byte(address, value as u8)?,
            1 => task.memory.write_bytes(address, &value.to_le_bytes())?,
            _ => return Err(program_error(line, "invalid native memory width")),
        }
        Ok(1)
    })
}

#[unsafe(no_mangle)]
pub(super) extern "C" fn strict_native_write_memory_offset(
    pointer: *mut c_void,
    width: i32,
    address: f64,
    offset: f64,
    value: f64,
    line: i32,
) -> i32 {
    strict_native_write_memory(pointer, width, address + offset, value, line)
}

#[unsafe(no_mangle)]
pub(super) extern "C" fn strict_native_write_string_memory(
    pointer: *mut c_void,
    address: f64,
    source: i32,
    line: i32,
) -> i32 {
    with_context(pointer, line, 0, |context, line| {
        let address = checked_guest_address(address, line)?;
        let value = context.string(source, line)?.to_vec();
        let terminator = address
            .checked_add(
                u32::try_from(value.len())
                    .map_err(|_| program_error(line, "indirect string address overflowed"))?,
            )
            .ok_or_else(|| program_error(line, "indirect string address overflowed"))?;
        // SAFETY: task is retained for this synchronous run.
        unsafe {
            (&mut *context.task).memory.write_bytes(address, &value)?;
            (&mut *context.task).memory.write_byte(terminator, b'\r')?;
        }
        Ok(1)
    })
}

#[unsafe(no_mangle)]
pub(super) extern "C" fn strict_native_input(
    pointer: *mut c_void,
    destination: i32,
    is_string: i32,
    line: i32,
) -> i32 {
    with_context(pointer, line, 0, |context, line| {
        // SAFETY: task and dispatcher are retained for this synchronous run.
        unsafe {
            (&mut *context.dispatcher).write_inline(&mut *context.task, b"? ")?;
            let mut input = SwiContext::default();
            input.registers[0] = INPUT_BUFFER;
            input.registers[1] = INPUT_BUFFER_SIZE - 1;
            input.registers[2] = u32::from(b' ');
            input.registers[3] = u32::from(u8::MAX);
            (&mut *context.dispatcher).dispatch(OS_READ_LINE, &mut *context.task, &mut input)?;
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
                .ok_or_else(|| program_error(line, "input address overflowed"))?;
            (&mut *context.task).memory.write_byte(terminator, 0)?;
            let bytes = (&*context.task)
                .memory
                .read_c_string(INPUT_BUFFER, INPUT_BUFFER_SIZE as usize)?;
            if is_string != 0 {
                let slot = usize::try_from(destination)
                    .map_err(|_| program_error(line, "compiled string slot is invalid"))?;
                if slot >= context.string_variable_count {
                    return Err(program_error(line, "INPUT target is not a string variable"));
                }
                context.string_values[slot] = bytes;
            } else {
                let text = String::from_utf8_lossy(&bytes);
                let value = text
                    .trim()
                    .parse::<f64>()
                    .map_err(|_| program_error(line, "INPUT value is not numeric"))?;
                let slot = usize::try_from(destination)
                    .map_err(|_| program_error(line, "compiled numeric slot is invalid"))?;
                let name = context
                    .numeric_names
                    .get(slot)
                    .ok_or_else(|| program_error(line, "compiled numeric slot is invalid"))?;
                context.numeric_slots[slot] = if name.ends_with('%') {
                    (value.trunc() as i32) as f64
                } else {
                    value
                };
            }
        }
        Ok(1)
    })
}

#[unsafe(no_mangle)]
pub(super) extern "C" fn strict_native_string_slice_assign(
    pointer: *mut c_void,
    target: i32,
    start: f64,
    length: f64,
    source: i32,
    line: i32,
) -> i32 {
    with_context(pointer, line, 0, |context, line| {
        let start = start.trunc().max(1.0) as usize - 1;
        let length = bounded_string_length(length, line)?;
        let replacement = context.string(source, line)?.to_vec();
        let target_slot = usize::try_from(target)
            .map_err(|_| program_error(line, "compiled string slot is invalid"))?;
        if target_slot >= context.string_variable_count {
            return Err(program_error(line, "string slice target is invalid"));
        }
        let value = &mut context.string_values[target_slot];
        let start = start.min(value.len());
        let end = start.saturating_add(length).min(value.len());
        value.splice(start..end, replacement.iter().copied().take(length));
        Ok(1)
    })
}

#[unsafe(no_mangle)]
pub(super) extern "C" fn strict_native_string_memory_byte(
    pointer: *mut c_void,
    address: f64,
    offset: f64,
    value: f64,
    line: i32,
) -> i32 {
    strict_native_write_memory_offset(pointer, 0, address, offset, value, line)
}

#[unsafe(no_mangle)]
pub(super) extern "C" fn strict_native_sys_unavailable(pointer: *mut c_void, line: i32) -> i32 {
    with_context(pointer, line, 0, |_context, line| {
        Err(program_error(
            line,
            "SYS lowering is unavailable in strict native mode",
        ))
    })
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
        1 => format!(
            "{number:.fractional_digits$e}",
            fractional_digits = precision.saturating_sub(1)
        )
        .replace('e', "E"),
        _ => format_number(number),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bounded_lengths_match_classic_runtime_checks() {
        assert_eq!(bounded_string_length(3.9, 1).unwrap(), 3);
        assert!(bounded_string_length(-1.0, 1).is_err());
    }
}
